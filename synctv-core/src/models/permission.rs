//! Permission System (Design Document 07-permission-system-design.md)
//!
//! This module implements a database-compatible permission bitmask system.
//!
//! Key features:
//! - Uses `u64` at API/domain boundaries while keeping defined permission bits
//!   in the PostgreSQL `BIGINT` range used by persistence
//! - Telegram-style permission inheritance
//! - Role and Status separation
//! - Allow/Deny permission pattern for customization

use serde::{Deserialize, Serialize};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};
use std::str::FromStr;

/// Permission bitmask.
///
/// The public protobuf/API representation is `uint64`, but persisted room
/// permission overrides use PostgreSQL `BIGINT`. Every defined product bit must
/// remain representable in signed storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionBits(pub u64);

impl PermissionBits {
    /// Send chat messages (includes messages with position for danmaku display)
    pub const SEND_CHAT: u64 = 1 << 0;

    /// Create media resources such as media items or playlists/folders, and
    /// edit media resources created by the actor.
    pub const CREATE_MEDIA_RESOURCE: u64 = 1 << 1;

    /// Delete media resources created by other users or resources without a
    /// recorded creator.
    pub const DELETE_MEDIA_RESOURCE_ANY: u64 = 1 << 2;

    /// Reorder media resources such as media items or playlists/folders.
    pub const REORDER_MEDIA_RESOURCES: u64 = 1 << 3;

    /// Clear media resource queues.
    pub const CLEAR_MEDIA_RESOURCES: u64 = 1 << 4;

    /// Start live stream (RTMP push)
    pub const START_LIVE: u64 = 1 << 5;

    /// Play control (play/pause/seek)
    pub const PLAY_CONTROL: u64 = 1 << 6;

    /// Switch current playback media
    pub const CHANGE_CURRENT_MEDIA: u64 = 1 << 7;

    /// Change playback rate
    pub const CHANGE_PLAYBACK_RATE: u64 = 1 << 8;

    /// Approve or reject pending join requests
    pub const APPROVE_MEMBER: u64 = 1 << 9;

    /// Kick member
    pub const KICK_MEMBER: u64 = 1 << 10;

    /// Ban/unban member
    pub const BAN_MEMBER: u64 = 1 << 11;

    /// Set member permissions
    pub const SET_MEMBER_PERMISSIONS: u64 = 1 << 12;

    /// Explicitly add a member when self-service joining is disabled
    pub const ADD_MEMBER: u64 = 1 << 13;

    /// Modify room settings
    pub const SET_ROOM_SETTINGS: u64 = 1 << 14;

    /// Delete chat messages
    pub const DELETE_CHAT: u64 = 1 << 15;

    /// Delete room
    pub const DELETE_ROOM: u64 = 1 << 16;

    /// View media resources such as media items and playlists/folders.
    pub const VIEW_MEDIA_RESOURCES: u64 = 1 << 17;

    /// Manage any media resource, including deleting resources created by
    /// others, reordering shared resource lists, and clearing resource queues.
    pub const MANAGE_MEDIA_RESOURCES: u64 = Self::DELETE_MEDIA_RESOURCE_ANY
        | Self::REORDER_MEDIA_RESOURCES
        | Self::CLEAR_MEDIA_RESOURCES;

    /// View member list
    pub const VIEW_MEMBER_LIST: u64 = 1 << 18;

    /// View chat history
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 19;

    /// Use WebRTC (voice/video)
    pub const USE_WEBRTC: u64 = 1 << 20;

    /// All permissions currently defined by the product model.
    pub const ALL: u64 = Self::SEND_CHAT
        | Self::CREATE_MEDIA_RESOURCE
        | Self::DELETE_MEDIA_RESOURCE_ANY
        | Self::REORDER_MEDIA_RESOURCES
        | Self::CLEAR_MEDIA_RESOURCES
        | Self::START_LIVE
        | Self::PLAY_CONTROL
        | Self::CHANGE_CURRENT_MEDIA
        | Self::CHANGE_PLAYBACK_RATE
        | Self::APPROVE_MEMBER
        | Self::KICK_MEMBER
        | Self::BAN_MEMBER
        | Self::SET_MEMBER_PERMISSIONS
        | Self::ADD_MEMBER
        | Self::SET_ROOM_SETTINGS
        | Self::DELETE_CHAT
        | Self::DELETE_ROOM
        | Self::VIEW_MEDIA_RESOURCES
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_WEBRTC;

    /// Defined permissions that cannot be delegated through room role/member
    /// permission overrides.
    pub const NON_ASSIGNABLE_IN_ROOM: u64 = Self::DELETE_ROOM;

    /// Permissions that can be delegated within a room to non-creator members.
    ///
    /// Room deletion is a lifecycle operation owned by the room creator or the
    /// global management plane. Unknown bits are rejected so raw bitmask update
    /// paths cannot grant undefined capabilities.
    pub const ASSIGNABLE_IN_ROOM: u64 = Self::ALL & !Self::NON_ASSIGNABLE_IN_ROOM;

    /// Default member permissions
    pub const DEFAULT_MEMBER: u64 = Self::SEND_CHAT
        | Self::CREATE_MEDIA_RESOURCE
        | Self::VIEW_MEDIA_RESOURCES
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_WEBRTC;

    /// Default admin permissions
    pub const DEFAULT_ADMIN: u64 = Self::DEFAULT_MEMBER
        | Self::DELETE_MEDIA_RESOURCE_ANY
        | Self::REORDER_MEDIA_RESOURCES
        | Self::CLEAR_MEDIA_RESOURCES
        | Self::START_LIVE
        | Self::PLAY_CONTROL
        | Self::CHANGE_CURRENT_MEDIA
        | Self::CHANGE_PLAYBACK_RATE
        | Self::APPROVE_MEMBER
        | Self::KICK_MEMBER
        | Self::BAN_MEMBER
        | Self::SET_MEMBER_PERMISSIONS
        | Self::ADD_MEMBER
        | Self::SET_ROOM_SETTINGS
        | Self::DELETE_CHAT;

    /// Default guest permissions.
    ///
    /// Guests can enter guest-enabled public rooms, but they do not receive
    /// media resource permissions by default.
    pub const DEFAULT_GUEST: u64 = Self::NONE;

    /// Permissions that can be granted to guests.
    ///
    /// Guests are not room members and cannot receive write or moderation
    /// capabilities. Playlist/media access is intentionally not included.
    pub const GUEST_ASSIGNABLE: u64 =
        Self::VIEW_MEMBER_LIST | Self::VIEW_CHAT_HISTORY | Self::USE_WEBRTC;

    pub const NONE: u64 = 0;

    #[must_use]
    pub const fn new(bits: u64) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self(Self::NONE)
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Check if has specific permission
    #[must_use]
    pub const fn has(&self, permission: u64) -> bool {
        (self.0 & permission) != 0
    }

    /// Check if has all specified permissions
    #[must_use]
    pub const fn has_all(&self, permissions: u64) -> bool {
        (self.0 & permissions) == permissions
    }

    /// Check if has any of the specified permissions
    #[must_use]
    pub const fn has_any(&self, permissions: u64) -> bool {
        (self.0 & permissions) != 0
    }

    /// Add permission (Allow pattern)
    pub const fn grant(&mut self, permission: u64) {
        self.0 |= permission;
    }

    /// Remove permission (Deny pattern)
    pub const fn revoke(&mut self, permission: u64) {
        self.0 &= !permission;
    }

    /// Set permission state
    pub const fn set(&mut self, permission: u64, enabled: bool) {
        if enabled {
            self.grant(permission);
        } else {
            self.revoke(permission);
        }
    }

    /// Toggle permission
    pub const fn toggle(&mut self, permission: u64) {
        self.0 ^= permission;
    }

    #[must_use]
    pub const fn includes_only_assignable_in_room(bits: u64) -> bool {
        bits & !Self::ASSIGNABLE_IN_ROOM == 0
    }
}

impl Default for PermissionBits {
    fn default() -> Self {
        Self::empty()
    }
}

impl From<u64> for PermissionBits {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<PermissionBits> for u64 {
    fn from(value: PermissionBits) -> Self {
        value.0
    }
}

impl BitOr for PermissionBits {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOr<u64> for PermissionBits {
    type Output = Self;

    fn bitor(self, rhs: u64) -> Self::Output {
        Self(self.0 | rhs)
    }
}

impl BitOrAssign for PermissionBits {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitOrAssign<u64> for PermissionBits {
    fn bitor_assign(&mut self, rhs: u64) {
        self.0 |= rhs;
    }
}

impl BitAnd for PermissionBits {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAnd<u64> for PermissionBits {
    type Output = Self;

    fn bitand(self, rhs: u64) -> Self::Output {
        Self(self.0 & rhs)
    }
}

impl BitAndAssign for PermissionBits {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitAndAssign<u64> for PermissionBits {
    fn bitand_assign(&mut self, rhs: u64) {
        self.0 &= rhs;
    }
}

impl BitXor for PermissionBits {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        Self(self.0 ^ rhs.0)
    }
}

impl BitXor<u64> for PermissionBits {
    type Output = Self;

    fn bitxor(self, rhs: u64) -> Self::Output {
        Self(self.0 ^ rhs)
    }
}

impl BitXorAssign for PermissionBits {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

impl BitXorAssign<u64> for PermissionBits {
    fn bitxor_assign(&mut self, rhs: u64) {
        self.0 ^= rhs;
    }
}

impl Not for PermissionBits {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

/// Room role preset (Telegram-style design)
///
/// These are the room-level roles that determine base permissions.
/// Custom permissions can be added/removed via Allow/Deny pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Room creator - has all permissions (fixed, cannot be modified)
    Creator,
    /// Room administrator - inherits from `DEFAULT_ADMIN` with possible custom overrides
    Admin,
    /// Regular member - inherits from `DEFAULT_MEMBER` with possible custom overrides
    Member,
    /// Guest - inherits from `DEFAULT_GUEST` with possible custom overrides
    Guest,
}

impl Role {
    /// Get base permissions for this role (before custom Allow/Deny modifications)
    #[must_use]
    pub const fn permissions(&self) -> PermissionBits {
        match self {
            Self::Creator => PermissionBits(PermissionBits::ALL),
            Self::Admin => PermissionBits(PermissionBits::DEFAULT_ADMIN),
            Self::Member => PermissionBits(PermissionBits::DEFAULT_MEMBER),
            Self::Guest => PermissionBits(PermissionBits::DEFAULT_GUEST),
        }
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "creator" => Ok(Self::Creator),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            "guest" => Ok(Self::Guest),
            other => Err(format!("Unknown role: {other}")),
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Creator => write!(f, "creator"),
            Self::Admin => write!(f, "admin"),
            Self::Member => write!(f, "member"),
            Self::Guest => write!(f, "guest"),
        }
    }
}

impl From<Role> for synctv_proto::common::RoomMemberRole {
    fn from(value: Role) -> Self {
        match value {
            Role::Creator => Self::Creator,
            Role::Admin => Self::Admin,
            Role::Member => Self::Member,
            Role::Guest => Self::Guest,
        }
    }
}

impl From<Role> for i32 {
    fn from(value: Role) -> Self {
        synctv_proto::common::RoomMemberRole::from(value) as Self
    }
}

impl TryFrom<synctv_proto::common::RoomMemberRole> for Role {
    type Error = String;

    fn try_from(value: synctv_proto::common::RoomMemberRole) -> Result<Self, Self::Error> {
        match value {
            synctv_proto::common::RoomMemberRole::Creator => Ok(Self::Creator),
            synctv_proto::common::RoomMemberRole::Admin => Ok(Self::Admin),
            synctv_proto::common::RoomMemberRole::Member => Ok(Self::Member),
            synctv_proto::common::RoomMemberRole::Guest => Ok(Self::Guest),
            synctv_proto::common::RoomMemberRole::Unspecified => {
                Err(format!("Unknown room member role: {}", value as i32))
            }
        }
    }
}

impl TryFrom<i32> for Role {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        let proto = synctv_proto::common::RoomMemberRole::try_from(value)
            .map_err(|_| format!("Unknown room member role: {value}"))?;
        Self::try_from(proto).map_err(|_| format!("Unknown room member role: {value}"))
    }
}

sqlx_i16_enum!(Role, "Invalid Role value", {
    Creator = 1,
    Admin = 2,
    Member = 3,
    Guest = 4,
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_has() {
        let perms = PermissionBits(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_grant_revoke() {
        let mut perms = PermissionBits::empty();
        perms.grant(PermissionBits::SEND_CHAT);
        perms.grant(PermissionBits::CREATE_MEDIA_RESOURCE);

        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));

        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_role_permissions() {
        let creator_perms = Role::Creator.permissions();
        assert!(creator_perms.has(PermissionBits::DELETE_ROOM));
        assert!(creator_perms.has(PermissionBits::SEND_CHAT));

        let member_perms = Role::Member.permissions();
        assert!(member_perms.has(PermissionBits::SEND_CHAT));
        assert!(member_perms.has(PermissionBits::USE_WEBRTC));
        assert!(!member_perms.has(PermissionBits::DELETE_ROOM));

        let guest_perms = Role::Guest.permissions();
        assert!(!guest_perms.has(PermissionBits::VIEW_MEDIA_RESOURCES));
        assert!(!guest_perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_allow_deny_pattern() {
        // Start with DEFAULT_MEMBER
        let mut perms = PermissionBits(PermissionBits::DEFAULT_MEMBER);

        // Add admin permission (Allow pattern)
        perms.grant(PermissionBits::BAN_MEMBER);
        assert!(perms.has(PermissionBits::BAN_MEMBER));

        // Remove chat permission (Deny pattern)
        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));

        // Other DEFAULT_MEMBER permissions remain
        assert!(perms.has(PermissionBits::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_set_enable_disable() {
        let mut perms = PermissionBits::empty();
        perms.set(PermissionBits::SEND_CHAT, true);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        perms.set(PermissionBits::SEND_CHAT, false);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_permission_toggle() {
        let mut perms = PermissionBits::empty();
        perms.toggle(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        perms.toggle(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
    }

    #[test]
    fn test_permission_bits_std_bit_ops() {
        let chat = PermissionBits::from(PermissionBits::SEND_CHAT);
        let media = PermissionBits::from(PermissionBits::CREATE_MEDIA_RESOURCE);
        let combined = chat | media;

        assert!(combined.has_all(PermissionBits::SEND_CHAT | PermissionBits::CREATE_MEDIA_RESOURCE));
        assert_eq!((combined & chat).bits(), PermissionBits::SEND_CHAT);
        assert_eq!(
            (combined ^ chat).bits(),
            PermissionBits::CREATE_MEDIA_RESOURCE
        );

        let mut assigned = PermissionBits::empty();
        assigned |= PermissionBits::SEND_CHAT;
        assigned |= media;
        assigned &= !chat;
        assert_eq!(assigned.bits(), PermissionBits::CREATE_MEDIA_RESOURCE);
    }

    #[test]
    fn test_room_assignable_permissions_reject_unknown_and_lifecycle_bits() {
        assert!(PermissionBits::includes_only_assignable_in_room(
            PermissionBits::SEND_CHAT | PermissionBits::CREATE_MEDIA_RESOURCE
        ));
        assert!(!PermissionBits::includes_only_assignable_in_room(
            PermissionBits::ALL | (1 << 21)
        ));
        assert!(!PermissionBits::includes_only_assignable_in_room(
            PermissionBits::DELETE_ROOM
        ));
    }

    #[test]
    fn test_permission_grant_idempotent() {
        let mut perms = PermissionBits::empty();
        perms.grant(PermissionBits::SEND_CHAT);
        perms.grant(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert_eq!(perms.0, PermissionBits::SEND_CHAT);
    }

    #[test]
    fn test_permission_revoke_idempotent() {
        let mut perms = PermissionBits::empty();
        perms.revoke(PermissionBits::SEND_CHAT); // no-op on empty
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert_eq!(perms.0, 0);
    }

    #[test]
    fn test_has_all_with_zero_is_vacuously_true() {
        let perms = PermissionBits::empty();
        assert!(perms.has_all(0)); // vacuously true: 0 & 0 == 0
    }

    #[test]
    fn test_has_any_with_zero_is_false() {
        let perms = PermissionBits(PermissionBits::ALL);
        assert!(!perms.has_any(0)); // 0 & anything == 0
    }

    #[test]
    fn test_role_from_str() {
        assert_eq!(Role::from_str("creator").unwrap(), Role::Creator);
        assert_eq!(Role::from_str("admin").unwrap(), Role::Admin);
        assert_eq!(Role::from_str("member").unwrap(), Role::Member);
        assert_eq!(Role::from_str("guest").unwrap(), Role::Guest);
        assert_eq!(Role::from_str("CREATOR").unwrap(), Role::Creator);
        assert_eq!(Role::from_str("Admin").unwrap(), Role::Admin);
        assert_eq!(Role::from_str(" member ").unwrap(), Role::Member);
    }

    #[test]
    fn test_role_from_str_invalid() {
        assert!(Role::from_str("superadmin").is_err());
        assert!(Role::from_str("").is_err());
        assert!(Role::from_str("owner").is_err());
    }

    #[test]
    fn test_role_display() {
        assert_eq!(Role::Creator.to_string(), "creator");
        assert_eq!(Role::Admin.to_string(), "admin");
        assert_eq!(Role::Member.to_string(), "member");
        assert_eq!(Role::Guest.to_string(), "guest");
    }

    #[test]
    fn test_role_display_roundtrip() {
        for role in [Role::Creator, Role::Admin, Role::Member, Role::Guest] {
            let display = role.to_string();
            let parsed = Role::from_str(&display).unwrap();
            assert_eq!(parsed, role);
        }
    }
}
