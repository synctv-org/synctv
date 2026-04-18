//! Permission System (Design Document 07-permission-system-design.md)
//!
//! This module implements the 64-bit permission bitmask system as specified in the design document.
//!
//! Key features:
//! - Uses u64 (not i64) for permission bits
//! - Telegram-style permission inheritance
//! - Role and Status separation
//! - Allow/Deny permission pattern for customization

use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// 64-bit permission bitmask (u64 as per design document)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionBits(pub u64);

impl PermissionBits {
    /// Send chat messages (includes messages with position for danmaku display)
    pub const SEND_CHAT: u64 = 1 << 0;

    /// Add media to a playlist
    pub const ADD_MEDIA: u64 = 1 << 1;

    /// Delete own media
    pub const DELETE_MEDIA_SELF: u64 = 1 << 2;

    /// Delete any media
    pub const DELETE_MEDIA_ANY: u64 = 1 << 3;

    /// Edit own media metadata
    pub const EDIT_MEDIA_SELF: u64 = 1 << 4;

    /// Edit any media metadata
    pub const EDIT_MEDIA_ANY: u64 = 1 << 5;

    /// Reorder playlist
    pub const REORDER_PLAYLIST: u64 = 1 << 6;

    /// Clear playlist
    pub const CLEAR_PLAYLIST: u64 = 1 << 7;

    /// Start live stream (RTMP push)
    pub const START_LIVE: u64 = 1 << 8;

    /// Reserved for future content management use (bit 9)
    pub const RESERVED_9: u64 = 1 << 9;

    /// Play control (play/pause/seek)
    pub const PLAY_CONTROL: u64 = 1 << 10;

    /// Switch current playback media
    pub const CHANGE_CURRENT_MEDIA: u64 = 1 << 11;

    /// Change playback rate
    pub const CHANGE_PLAYBACK_RATE: u64 = 1 << 12;

    /// Approve or reject pending join requests
    pub const APPROVE_MEMBER: u64 = 1 << 20;

    /// Kick member
    pub const KICK_MEMBER: u64 = 1 << 21;

    /// Ban/unban member
    pub const BAN_MEMBER: u64 = 1 << 22;

    /// Set member permissions
    pub const SET_MEMBER_PERMISSIONS: u64 = 1 << 23;

    /// Explicitly add a member when self-service joining is disabled
    pub const ADD_MEMBER: u64 = 1 << 24;

    /// Modify room settings
    pub const SET_ROOM_SETTINGS: u64 = 1 << 30;

    /// Delete chat messages
    pub const DELETE_CHAT: u64 = 1 << 32;

    /// Delete room
    pub const DELETE_ROOM: u64 = 1 << 35;

    /// Permissions that can be delegated within a room to non-creator members.
    ///
    /// Room deletion is a lifecycle operation owned by the room creator or the
    /// global management plane, not a room-scoped capability that should be
    /// granted through role overrides or member permission editing.
    pub const ASSIGNABLE_IN_ROOM: u64 = Self::ALL & !Self::DELETE_ROOM;

    /// View playlist
    pub const VIEW_PLAYLIST: u64 = 1 << 40;

    /// View member list
    pub const VIEW_MEMBER_LIST: u64 = 1 << 41;

    /// View chat history
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 42;

    /// Use WebRTC (voice/video)
    pub const USE_WEBRTC: u64 = 1 << 50;

    // Reserved for future use

    /// All permissions (for Creator)
    pub const ALL: u64 = u64::MAX;

    /// Default member permissions
    pub const DEFAULT_MEMBER: u64 = Self::SEND_CHAT
        | Self::ADD_MEDIA
        | Self::DELETE_MEDIA_SELF
        | Self::EDIT_MEDIA_SELF
        | Self::VIEW_PLAYLIST
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY;

    /// Default admin permissions
    pub const DEFAULT_ADMIN: u64 = Self::DEFAULT_MEMBER
        | Self::DELETE_MEDIA_ANY
        | Self::EDIT_MEDIA_ANY
        | Self::REORDER_PLAYLIST
        | Self::CLEAR_PLAYLIST
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
        | Self::USE_WEBRTC;

    /// Default guest permissions (read-only)
    pub const DEFAULT_GUEST: u64 = Self::VIEW_PLAYLIST;

    pub const NONE: u64 = 0;

    #[must_use]
    pub const fn new(bits: u64) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn empty() -> Self {
        Self(Self::NONE)
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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "creator" => Ok(Self::Creator),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            "guest" => Ok(Self::Guest),
            _ => Err(format!("Unknown role: {s}")),
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

// Database mapping: Role -> SMALLINT (1=creator, 2=admin, 3=member, 4=guest)
impl sqlx::Type<sqlx::Postgres> for Role {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for Role {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Creator => 1,
            Self::Admin => 2,
            Self::Member => 3,
            Self::Guest => 4,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Role {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let val = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        match val {
            1 => Ok(Self::Creator),
            2 => Ok(Self::Admin),
            3 => Ok(Self::Member),
            4 => Ok(Self::Guest),
            _ => Err(format!("Invalid Role value: {val}").into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_has() {
        let perms = PermissionBits(PermissionBits::SEND_CHAT);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(!perms.has(PermissionBits::ADD_MEDIA));
    }

    #[test]
    fn test_permission_grant_revoke() {
        let mut perms = PermissionBits::empty();
        perms.grant(PermissionBits::SEND_CHAT);
        perms.grant(PermissionBits::ADD_MEDIA);

        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MEDIA));

        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MEDIA));
    }

    #[test]
    fn test_role_permissions() {
        let creator_perms = Role::Creator.permissions();
        assert!(creator_perms.has(PermissionBits::DELETE_ROOM));
        assert!(creator_perms.has(PermissionBits::SEND_CHAT));

        let member_perms = Role::Member.permissions();
        assert!(member_perms.has(PermissionBits::SEND_CHAT));
        assert!(!member_perms.has(PermissionBits::DELETE_ROOM));

        let guest_perms = Role::Guest.permissions();
        assert!(guest_perms.has(PermissionBits::VIEW_PLAYLIST));
        assert!(!guest_perms.has(PermissionBits::ADD_MEDIA));
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
        assert!(perms.has(PermissionBits::ADD_MEDIA));
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
