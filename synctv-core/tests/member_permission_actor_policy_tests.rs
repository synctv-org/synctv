//! Regression tests for the member permission override actor policy:
//! nobody may write their own overrides, and overrides on elevated members
//! (admin or creator) are writable only by the room creator.

mod permission_test_support;

use synctv_core::{
    Error,
    models::{RoomAdminPermissionBits, RoomMemberPermissionBits, RoomRole},
    repository::UserRepository,
    service::{MemberPermissionPatch, UpdateMemberWithOutboxRequest},
};
use synctv_core_testing::{create_test_pool, ok};

use permission_test_support::{make_room_service, make_user};

fn permission_patch(
    admin_added_permissions: u64,
    admin_removed_permissions: u64,
) -> MemberPermissionPatch {
    MemberPermissionPatch {
        apply_permission_update: true,
        added_permissions: 0,
        removed_permissions: 0,
        admin_added_permissions,
        admin_removed_permissions,
    }
}

fn member_patch(added_permissions: u64) -> MemberPermissionPatch {
    MemberPermissionPatch {
        apply_permission_update: true,
        added_permissions,
        removed_permissions: 0,
        admin_added_permissions: 0,
        admin_removed_permissions: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_self_grant_delete_room() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("actor_self_creator")).await,
        "creator should be created",
    );
    let admin = ok(
        user_repo.create(&make_user("actor_self_admin")).await,
        "admin should be created",
    );

    let room = ok(
        room_service
            .create_room(
                "Actor Policy Self Grant Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, admin.id, None).await,
        "admin should join room",
    );
    ok(
        room_service
            .member_service()
            .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
            .await,
        "creator should promote admin",
    );

    let result = room_service
        .update_member_with_outbox(UpdateMemberWithOutboxRequest {
            room_id: room.id,
            actor_id: admin.id,
            target_user_id: admin.id,
            role: None,
            permissions: permission_patch(RoomAdminPermissionBits::DELETE_ROOM, 0),
            outbox_event_factory: None,
        })
        .await;
    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "admin self-granting DELETE_ROOM must be rejected, got {result:?}"
    );

    let deletion = room_service.delete_room(room.id, admin.id).await;
    assert!(
        matches!(deletion, Err(Error::Authorization(_))),
        "admin without DELETE_ROOM must not delete the creator's room"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_modify_other_admin_or_creator_overrides() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("actor_pair_creator")).await,
        "creator should be created",
    );
    let admin = ok(
        user_repo.create(&make_user("actor_pair_admin")).await,
        "admin should be created",
    );
    let other_admin = ok(
        user_repo.create(&make_user("actor_pair_other")).await,
        "other admin should be created",
    );

    let room = ok(
        room_service
            .create_room(
                "Actor Policy Pair Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    for user in [&admin, &other_admin] {
        ok(
            room_service.join_room(room.id, user.id, None).await,
            "admin should join room",
        );
        ok(
            room_service
                .member_service()
                .set_member_role(room.id, creator.id, user.id, RoomRole::Admin)
                .await,
            "creator should promote admin",
        );
    }

    let strip = room_service
        .update_member_with_outbox(UpdateMemberWithOutboxRequest {
            room_id: room.id,
            actor_id: admin.id,
            target_user_id: other_admin.id,
            role: None,
            permissions: permission_patch(0, RoomAdminPermissionBits::ALL),
            outbox_event_factory: None,
        })
        .await;
    assert!(
        matches!(strip, Err(Error::Authorization(_))),
        "admin stripping another admin must be rejected, got {strip:?}"
    );

    let touch_creator = room_service
        .update_member_with_outbox(UpdateMemberWithOutboxRequest {
            room_id: room.id,
            actor_id: admin.id,
            target_user_id: creator.id,
            role: None,
            permissions: permission_patch(RoomAdminPermissionBits::USE_VOICE_CHAT, 0),
            outbox_event_factory: None,
        })
        .await;
    assert!(
        matches!(touch_creator, Err(Error::Authorization(_))),
        "admin touching creator overrides must be rejected, got {touch_creator:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_creator_can_delegate_delete_room_and_admin_manages_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("actor_ok_creator")).await,
        "creator should be created",
    );
    let admin = ok(
        user_repo.create(&make_user("actor_ok_admin")).await,
        "admin should be created",
    );
    let member = ok(
        user_repo.create(&make_user("actor_ok_member")).await,
        "member should be created",
    );

    let room = ok(
        room_service
            .create_room(
                "Actor Policy Legitimate Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, admin.id, None).await,
        "admin should join room",
    );
    ok(
        room_service.join_room(room.id, member.id, None).await,
        "member should join room",
    );
    ok(
        room_service
            .member_service()
            .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
            .await,
        "creator should promote admin",
    );

    // Admin may still manage a plain member's overrides.
    ok(
        room_service
            .update_member_with_outbox(UpdateMemberWithOutboxRequest {
                room_id: room.id,
                actor_id: admin.id,
                target_user_id: member.id,
                role: None,
                permissions: member_patch(RoomMemberPermissionBits::USE_VOICE_CHAT),
                outbox_event_factory: None,
            })
            .await,
        "admin should manage a plain member's overrides",
    );

    // Creator may delegate DELETE_ROOM to an admin, who can then delete.
    let delegated = ok(
        room_service
            .update_member_with_outbox(UpdateMemberWithOutboxRequest {
                room_id: room.id,
                actor_id: creator.id,
                target_user_id: admin.id,
                role: None,
                permissions: permission_patch(RoomAdminPermissionBits::DELETE_ROOM, 0),
                outbox_event_factory: None,
            })
            .await,
        "creator should be able to delegate DELETE_ROOM to an admin",
    );
    assert_ne!(
        delegated.admin_added_permissions & RoomAdminPermissionBits::DELETE_ROOM,
        0,
        "creator-granted DELETE_ROOM should persist in admin_added_permissions"
    );

    ok(
        room_service.delete_room(room.id, admin.id).await,
        "admin with creator-granted DELETE_ROOM should be able to delete",
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_legacy_permission_writers_reject_self_edit() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("actor_legacy_creator")).await,
        "creator should be created",
    );
    let admin = ok(
        user_repo.create(&make_user("actor_legacy_admin")).await,
        "admin should be created",
    );

    let room = ok(
        room_service
            .create_room(
                "Actor Policy Legacy Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, admin.id, None).await,
        "admin should join room",
    );
    ok(
        room_service
            .member_service()
            .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
            .await,
        "creator should promote admin",
    );

    let member_service = room_service.member_service();
    let grant = member_service
        .grant_permission(
            room.id,
            admin.id,
            admin.id,
            RoomAdminPermissionBits::DELETE_ROOM,
        )
        .await;
    assert!(
        matches!(grant, Err(Error::Authorization(_))),
        "legacy grant_permission self-edit must be rejected, got {grant:?}"
    );

    let set = room_service
        .set_member_permission(
            room.id,
            admin.id,
            admin.id,
            0,
            RoomAdminPermissionBits::DELETE_ROOM,
        )
        .await;
    assert!(
        matches!(set, Err(Error::Authorization(_))),
        "legacy set_member_permission self-edit must be rejected, got {set:?}"
    );

    let revoke = member_service
        .revoke_permission(room.id, admin.id, admin.id, RoomAdminPermissionBits::ALL)
        .await;
    assert!(
        matches!(revoke, Err(Error::Authorization(_))),
        "legacy revoke_permission self-edit must be rejected, got {revoke:?}"
    );

    let remanage = member_service
        .remanage_member_permissions(room.id, admin.id, admin.id)
        .await;
    assert!(
        matches!(remanage, Err(Error::Authorization(_))),
        "legacy remanage_member_permissions self-edit must be rejected, got {remanage:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_admin_cannot_escalate_via_room_settings_admin_baseline() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = ok(
        user_repo.create(&make_user("baseline_creator")).await,
        "creator should be created",
    );
    let admin = ok(
        user_repo.create(&make_user("baseline_admin")).await,
        "admin should be created",
    );

    let room = ok(
        room_service
            .create_room(
                "Settings Baseline Room".to_string(),
                String::new(),
                creator.id,
                None,
                None,
            )
            .await,
        "room should be created",
    );
    ok(
        room_service.join_room(room.id, admin.id, None).await,
        "admin should join room",
    );
    ok(
        room_service
            .member_service()
            .set_member_role(room.id, creator.id, admin.id, RoomRole::Admin)
            .await,
        "creator should promote admin",
    );

    // Admin rewrites the room-wide admin baseline to grant DELETE_ROOM to
    // every admin (themselves included) through MANAGE_ROOM_SETTINGS.
    let mut escalated = ok(
        room_service.get_room_settings(&room.id).await,
        "room settings should load",
    );
    escalated.admin_added_permissions =
        synctv_core::models::room_settings::AdminAddedPermissions::new(
            RoomAdminPermissionBits::DELETE_ROOM,
        );
    let result = room_service
        .set_settings(room.id, admin.id, escalated)
        .await;
    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "admin changing the admin permission baseline must be rejected, got {result:?}"
    );

    let deletion = room_service.delete_room(room.id, admin.id).await;
    assert!(
        matches!(deletion, Err(Error::Authorization(_))),
        "admin must not gain DELETE_ROOM through the settings baseline"
    );

    // Admin may still tune member-level defaults within existing powers.
    let mut member_tweak = ok(
        room_service.get_room_settings(&room.id).await,
        "room settings should load",
    );
    member_tweak.member_removed_permissions =
        synctv_core::models::room_settings::MemberRemovedPermissions::new(
            RoomMemberPermissionBits::USE_VOICE_CHAT,
        );
    ok(
        room_service
            .set_settings(room.id, admin.id, member_tweak)
            .await,
        "admin should adjust member-level room settings",
    );

    // The creator may set the admin baseline deliberately.
    let mut delegated = ok(
        room_service.get_room_settings(&room.id).await,
        "room settings should load",
    );
    delegated.admin_added_permissions =
        synctv_core::models::room_settings::AdminAddedPermissions::new(
            RoomAdminPermissionBits::DELETE_ROOM,
        );
    ok(
        room_service
            .set_settings(room.id, creator.id, delegated)
            .await,
        "creator should set the admin permission baseline",
    );
    ok(
        room_service.delete_room(room.id, admin.id).await,
        "admin with creator-granted baseline DELETE_ROOM should delete",
    );
}
