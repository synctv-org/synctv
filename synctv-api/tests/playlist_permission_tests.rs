//! Media resource permission tests.
//!
//! Media items and playlists/folders are the same product resource family.
//! Permissions should describe the user capability, not the storage table.

#![allow(clippy::unwrap_used)]

use synctv_core::models::permission::PermissionBits;

const _: () = assert!(
    PermissionBits::CREATE_MEDIA_RESOURCE > 0,
    "CREATE_MEDIA_RESOURCE permission must have a non-zero value"
);

const _: () = assert!(
    PermissionBits::VIEW_MEDIA_RESOURCES > 0,
    "VIEW_MEDIA_RESOURCES permission must have a non-zero value"
);

#[test]
fn test_media_resource_permissions_are_distinct_from_chat() {
    assert_ne!(
        PermissionBits::CREATE_MEDIA_RESOURCE,
        PermissionBits::SEND_CHAT
    );
    assert_ne!(
        PermissionBits::VIEW_MEDIA_RESOURCES,
        PermissionBits::SEND_CHAT
    );
    assert_ne!(
        PermissionBits::REORDER_MEDIA_RESOURCES,
        PermissionBits::CREATE_MEDIA_RESOURCE
    );
}

#[test]
fn test_member_default_can_view_create_and_edit_own_resources() {
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);

    assert!(member_default.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    assert!(member_default.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    assert!(!member_default.has(PermissionBits::DELETE_MEDIA_RESOURCE_ANY));
    assert!(!member_default.has(PermissionBits::REORDER_MEDIA_RESOURCES));
    assert!(!member_default.has(PermissionBits::CLEAR_MEDIA_RESOURCES));
}

#[test]
fn test_admin_default_can_manage_shared_resources() {
    let admin_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);

    assert!(admin_default.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    assert!(admin_default.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    assert!(admin_default.has(PermissionBits::DELETE_MEDIA_RESOURCE_ANY));
    assert!(admin_default.has(PermissionBits::REORDER_MEDIA_RESOURCES));
    assert!(admin_default.has(PermissionBits::CLEAR_MEDIA_RESOURCES));
}

#[test]
fn test_guest_cannot_receive_media_resource_permissions() {
    let guest_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
    let requested = PermissionBits(
        PermissionBits::VIEW_MEDIA_RESOURCES
            | PermissionBits::CREATE_MEDIA_RESOURCE
            | PermissionBits::REORDER_MEDIA_RESOURCES,
    );

    let effective = PermissionBits(
        (guest_default.0 & PermissionBits::GUEST_ASSIGNABLE)
            | (requested.0 & PermissionBits::GUEST_ASSIGNABLE),
    );

    assert!(!effective.has(PermissionBits::VIEW_MEDIA_RESOURCES));
    assert!(!effective.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    assert!(!effective.has(PermissionBits::REORDER_MEDIA_RESOURCES));
}

#[test]
fn test_document_media_resource_permission_requirements() {
    let view_media_or_playlist = PermissionBits::VIEW_MEDIA_RESOURCES;
    let create_and_edit_own_media_or_playlist = PermissionBits::CREATE_MEDIA_RESOURCE;
    let delete_foreign_media_or_playlist = PermissionBits::DELETE_MEDIA_RESOURCE_ANY;
    let move_media_or_playlist = PermissionBits::REORDER_MEDIA_RESOURCES;
    let clear_resource_queue = PermissionBits::CLEAR_MEDIA_RESOURCES;

    assert_ne!(
        view_media_or_playlist,
        create_and_edit_own_media_or_playlist
    );
    assert_ne!(
        create_and_edit_own_media_or_playlist,
        move_media_or_playlist
    );
    assert_ne!(
        create_and_edit_own_media_or_playlist,
        delete_foreign_media_or_playlist
    );
    assert_eq!(
        PermissionBits::MANAGE_MEDIA_RESOURCES,
        delete_foreign_media_or_playlist | move_media_or_playlist | clear_resource_queue
    );
}
