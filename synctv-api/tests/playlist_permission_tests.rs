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
    assert_ne!(
        RoomPermission::MANAGE_OWN_MEDIA,
        RoomPermission::SEND_CHAT_MESSAGES
    );
    assert_ne!(
        RoomPermission::BROWSE_LIBRARY,
        RoomPermission::SEND_CHAT_MESSAGES
    );
    assert_ne!(
        RoomPermission::REORDER_MEDIA,
        RoomPermission::MANAGE_OWN_MEDIA
    );
}

#[test]
fn test_member_default_can_view_create_and_edit_own_resources() {
    let member_default = RoomPermissionSet::default_member();

    assert!(member_default.has(RoomPermission::BROWSE_LIBRARY));
    assert!(member_default.has(RoomPermission::MANAGE_OWN_MEDIA));
    assert!(!member_default.has(RoomPermission::DELETE_MEDIA));
    assert!(!member_default.has(RoomPermission::REORDER_MEDIA));
    assert!(!member_default.has(RoomPermission::CLEAR_MEDIA));
}

#[test]
fn test_admin_default_can_manage_shared_resources() {
    let admin_default = RoomPermissionSet::default_admin();

    assert!(admin_default.has(RoomPermission::BROWSE_LIBRARY));
    assert!(admin_default.has(RoomPermission::MANAGE_OWN_MEDIA));
    assert!(admin_default.has(RoomPermission::DELETE_MEDIA));
    assert!(admin_default.has(RoomPermission::REORDER_MEDIA));
    assert!(admin_default.has(RoomPermission::CLEAR_MEDIA));
    assert!(!admin_default.has(RoomPermission::DELETE_ROOM));
}

#[test]
fn test_guest_cannot_receive_media_resource_permissions() {
    let guest_default = RoomPermissionSet::default_guest();
    let requested = RoomPermissionSet(
        RoomAdminPermissionBits::BROWSE_LIBRARY
            | RoomAdminPermissionBits::MANAGE_OWN_MEDIA
            | RoomAdminPermissionBits::REORDER_MEDIA,
    );

    let guest_assignable = RoomGuestPermissionBits::to_permissions(RoomGuestPermissionBits::ALL);
    let effective =
        RoomPermissionSet((guest_default.0 & guest_assignable) | (requested.0 & guest_assignable));

    assert!(!effective.has(RoomPermission::BROWSE_LIBRARY));
    assert!(!effective.has(RoomPermission::MANAGE_OWN_MEDIA));
    assert!(!effective.has(RoomPermission::REORDER_MEDIA));
}

#[test]
fn test_document_media_resource_permission_requirements() {
    let browse_library_or_playlist = RoomPermission::BROWSE_LIBRARY;
    let create_and_edit_own_media_or_playlist = RoomPermission::MANAGE_OWN_MEDIA;
    let delete_foreign_media_or_playlist = RoomPermission::DELETE_MEDIA;
    let move_media_or_playlist = RoomPermission::REORDER_MEDIA;
    let clear_resource_queue = RoomPermission::CLEAR_MEDIA;

    assert_ne!(
        browse_library_or_playlist,
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
        | RoomAdminPermissionBits::DELETE_MEDIA
        | RoomAdminPermissionBits::REORDER_MEDIA
        | RoomAdminPermissionBits::CLEAR_MEDIA;
    assert!(manage_media_resources.has(delete_foreign_media_or_playlist));
    assert!(manage_media_resources.has(move_media_or_playlist));
    assert!(manage_media_resources.has(clear_resource_queue));
}
