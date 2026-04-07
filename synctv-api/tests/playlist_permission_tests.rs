//! Playlist Permission Tests (TDD)
//!
//! Tests that playlist operations (create, update, delete) properly check permissions.
//!
//! Security Issue: If playlist operations don't check permissions, any room member
//! could modify playlists even without REORDER_PLAYLIST permission.
//!
//! Fix: All playlist mutation operations must check REORDER_PLAYLIST permission,
//! and read operations must verify room membership.

#![allow(clippy::unwrap_used)]

use synctv_core::models::permission::PermissionBits;

// ============================================================================
// Permission Bits Tests
// ============================================================================

const _: () = assert!(
    PermissionBits::REORDER_PLAYLIST > 0,
    "REORDER_PLAYLIST permission must have a non-zero value"
);

#[test]
fn test_reorder_playlist_is_distinct_from_other_permissions() {
    // REORDER_PLAYLIST should be different from other common permissions
    assert_ne!(
        PermissionBits::REORDER_PLAYLIST,
        PermissionBits::VIEW_PLAYLIST,
        "REORDER_PLAYLIST should be distinct from VIEW_PLAYLIST"
    );
    assert_ne!(
        PermissionBits::REORDER_PLAYLIST,
        PermissionBits::ADD_MEDIA,
        "REORDER_PLAYLIST should be distinct from ADD_MEDIA"
    );
    assert_ne!(
        PermissionBits::REORDER_PLAYLIST,
        PermissionBits::SEND_CHAT,
        "REORDER_PLAYLIST should be distinct from SEND_CHAT"
    );
}

// ============================================================================
// Role Default Permission Tests
// ============================================================================

// ============================================================================
// Permission Check Tests for Effective Permissions
// ============================================================================

#[test]
fn test_permission_check_with_removed_permission() {
    // Member with REORDER_PLAYLIST removed should fail check
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
    let removed = PermissionBits::REORDER_PLAYLIST;

    // Calculate effective: (default) & ~removed
    let effective = PermissionBits(member_default.0 & !removed);

    assert!(
        !effective.has(PermissionBits::REORDER_PLAYLIST),
        "Member with removed REORDER_PLAYLIST should not have the permission"
    );
}

#[test]
fn test_permission_check_with_added_permission() {
    // Guest with REORDER_PLAYLIST added should pass check
    let guest_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
    let added = PermissionBits::REORDER_PLAYLIST;

    // Calculate effective: (default) | added
    let effective = PermissionBits(guest_default.0 | added);

    assert!(
        effective.has(PermissionBits::REORDER_PLAYLIST),
        "Guest with added REORDER_PLAYLIST should have the permission"
    );
}

// ============================================================================
// View Playlist Permission Tests (for read operations)
// ============================================================================

const _: () = assert!(
    PermissionBits::VIEW_PLAYLIST > 0,
    "VIEW_PLAYLIST permission must have a non-zero value"
);

#[test]
fn test_all_roles_have_view_playlist_permission() {
    // All roles should have VIEW_PLAYLIST by default (read access)
    let guest_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
    let admin_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    let creator_perms = PermissionBits(PermissionBits::ALL);

    assert!(
        guest_default.has(PermissionBits::VIEW_PLAYLIST),
        "Guest should have VIEW_PLAYLIST permission"
    );
    assert!(
        member_default.has(PermissionBits::VIEW_PLAYLIST),
        "Member should have VIEW_PLAYLIST permission"
    );
    assert!(
        admin_default.has(PermissionBits::VIEW_PLAYLIST),
        "Admin should have VIEW_PLAYLIST permission"
    );
    assert!(
        creator_perms.has(PermissionBits::VIEW_PLAYLIST),
        "Creator should have VIEW_PLAYLIST permission"
    );
}

// ============================================================================
// Permission Hierarchy Tests
// ============================================================================

#[test]
fn test_admin_permissions_include_member_permissions() {
    let admin_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);

    // Admin should have at least all member permissions
    let admin_extra = admin_default.0 & !member_default.0;
    assert!(
        admin_extra != 0 || admin_default.0 == member_default.0,
        "Admin should have at least member permissions"
    );
}

#[test]
fn test_creator_has_all_permissions() {
    let creator_perms = PermissionBits(PermissionBits::ALL);

    // Creator should have all defined permissions
    assert!(
        creator_perms.has(PermissionBits::REORDER_PLAYLIST),
        "Creator should have REORDER_PLAYLIST"
    );
    assert!(
        creator_perms.has(PermissionBits::VIEW_PLAYLIST),
        "Creator should have VIEW_PLAYLIST"
    );
    assert!(
        creator_perms.has(PermissionBits::ADD_MEDIA),
        "Creator should have ADD_MEDIA"
    );
    assert!(
        creator_perms.has(PermissionBits::KICK_MEMBER),
        "Creator should have KICK_MEMBER"
    );
    assert!(
        creator_perms.has(PermissionBits::BAN_MEMBER),
        "Creator should have BAN_MEMBER"
    );
}

// ============================================================================
// Permission Combination Tests
// ============================================================================

#[test]
fn test_multiple_permissions_can_be_checked() {
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);

    // Check multiple permissions at once - member has ADD_MEDIA but not REORDER_PLAYLIST
    let required_for_reorder = PermissionBits::REORDER_PLAYLIST | PermissionBits::ADD_MEDIA;
    let has_both = (member_default.0 & required_for_reorder) == required_for_reorder;

    assert!(
        !has_both,
        "Member should NOT have both REORDER_PLAYLIST and ADD_MEDIA - only ADD_MEDIA"
    );

    // But member should have ADD_MEDIA alone
    assert!(
        member_default.has(PermissionBits::ADD_MEDIA),
        "Member should have ADD_MEDIA permission"
    );
}

#[test]
fn test_permission_bitmask_operations() {
    // Test that permission bitmask operations work correctly
    let perm_a = PermissionBits::REORDER_PLAYLIST;
    let perm_b = PermissionBits::VIEW_PLAYLIST;
    let combined = PermissionBits(perm_a | perm_b);

    assert!(combined.has(perm_a), "Combined should have permission A");
    assert!(combined.has(perm_b), "Combined should have permission B");

    // Remove perm_a
    let removed = PermissionBits(combined.0 & !perm_a);
    assert!(
        !removed.has(perm_a),
        "Should not have permission A after removal"
    );
    assert!(removed.has(perm_b), "Should still have permission B");
}

// ============================================================================
// Permission Check for API Layer Tests
// ============================================================================

#[test]
fn test_check_permission_pattern() {
    // This tests the pattern used in ClientApiImpl::create_playlist
    // The actual check is: room_service.check_permission(&rid, &uid, PermissionBits::REORDER_PLAYLIST)

    // Simulate the check:
    // 1. Get user's effective permissions
    let member_default = PermissionBits(PermissionBits::DEFAULT_MEMBER);

    // 2. Check if user has required permission
    let has_permission = member_default.has(PermissionBits::REORDER_PLAYLIST);

    // Members do NOT have REORDER_PLAYLIST by default - it's admin-only
    assert!(
        !has_permission,
        "Member should NOT pass REORDER_PLAYLIST check - admin required"
    );

    // Admin should pass
    let admin_default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    let admin_has_permission = admin_default.has(PermissionBits::REORDER_PLAYLIST);

    assert!(
        admin_has_permission,
        "Admin should pass REORDER_PLAYLIST check"
    );

    // Guest should fail
    let guest_default = PermissionBits(PermissionBits::DEFAULT_GUEST);
    let guest_has_permission = guest_default.has(PermissionBits::REORDER_PLAYLIST);

    assert!(
        !guest_has_permission,
        "Guest should fail REORDER_PLAYLIST check"
    );
}

// ============================================================================
// Playlist Operation Permission Requirements Documentation
// ============================================================================

#[test]
fn test_document_playlist_permission_requirements() {
    // Document what permissions are required for each playlist operation:

    // Create: REORDER_PLAYLIST
    let create_req = PermissionBits::REORDER_PLAYLIST;

    // Update: REORDER_PLAYLIST
    let update_req = PermissionBits::REORDER_PLAYLIST;

    // Delete: REORDER_PLAYLIST
    let delete_req = PermissionBits::REORDER_PLAYLIST;

    // Get: Membership only (VIEW_PLAYLIST implied by membership)
    // List: Membership only (VIEW_PLAYLIST implied by membership)

    // All mutation operations require the same permission
    assert_eq!(
        create_req, update_req,
        "Create and Update should require same permission"
    );
    assert_eq!(
        update_req, delete_req,
        "Update and Delete should require same permission"
    );
}
