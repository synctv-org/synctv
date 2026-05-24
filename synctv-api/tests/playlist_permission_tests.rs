//! Media resource permission tests.
//!
//! Media items and playlists/folders are the same product resource family.
//! Permissions should describe the user capability, not the storage table.

#![allow(clippy::unwrap_used)]

use synctv_core::models::{
    RoomAdminPermissionBits, RoomGuestPermissionBits, RoomPermission, RoomPermissionSet,
};

#[test]
fn test_media_resource_permissions_are_distinct_from_chat() {
    assert_ne!(RoomPermission::CREATE_MEDIA_RESOURCE, RoomPermission::CHAT);
    assert_ne!(RoomPermission::VIEW_MEDIA_RESOURCES, RoomPermission::CHAT);
    assert_ne!(
        RoomPermission::REORDER_MEDIA_RESOURCES,
        RoomPermission::CREATE_MEDIA_RESOURCE
    );
}

#[test]
fn test_member_default_can_view_create_and_edit_own_resources() {
    let member_default = RoomPermissionSet::default_member();

    assert!(member_default.has(RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(member_default.has(RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(!member_default.has(RoomPermission::DELETE_MEDIA_RESOURCE_ANY));
    assert!(!member_default.has(RoomPermission::REORDER_MEDIA_RESOURCES));
    assert!(!member_default.has(RoomPermission::CLEAR_MEDIA_RESOURCES));
}

#[test]
fn test_admin_default_can_manage_shared_resources() {
    let admin_default = RoomPermissionSet::default_admin();

    assert!(admin_default.has(RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(admin_default.has(RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(admin_default.has(RoomPermission::DELETE_MEDIA_RESOURCE_ANY));
    assert!(admin_default.has(RoomPermission::REORDER_MEDIA_RESOURCES));
    assert!(admin_default.has(RoomPermission::CLEAR_MEDIA_RESOURCES));
    assert!(!admin_default.has(RoomPermission::DELETE_ROOM));
}

#[test]
fn test_guest_cannot_receive_media_resource_permissions() {
    let guest_default = RoomPermissionSet::default_guest();
    let requested = RoomPermissionSet(
        RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES
            | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
            | RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES,
    );

    let guest_assignable = RoomGuestPermissionBits::to_permissions(RoomGuestPermissionBits::ALL);
    let effective =
        RoomPermissionSet((guest_default.0 & guest_assignable) | (requested.0 & guest_assignable));

    assert!(!effective.has(RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(!effective.has(RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(!effective.has(RoomPermission::REORDER_MEDIA_RESOURCES));
}

#[test]
fn test_document_media_resource_permission_requirements() {
    let view_media_or_playlist = RoomPermission::VIEW_MEDIA_RESOURCES;
    let create_and_edit_own_media_or_playlist = RoomPermission::CREATE_MEDIA_RESOURCE;
    let delete_foreign_media_or_playlist = RoomPermission::DELETE_MEDIA_RESOURCE_ANY;
    let move_media_or_playlist = RoomPermission::REORDER_MEDIA_RESOURCES;
    let clear_resource_queue = RoomPermission::CLEAR_MEDIA_RESOURCES;

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
    let manage_media_resources = RoomPermissionSet::empty()
        | RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY
        | RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES
        | RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES;
    assert!(manage_media_resources.has(delete_foreign_media_or_playlist));
    assert!(manage_media_resources.has(move_media_or_playlist));
    assert!(manage_media_resources.has(clear_resource_queue));
}
