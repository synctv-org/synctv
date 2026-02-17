//! Permission System (Design Document 07-权限系统设计.md)
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
    // ===== Content Management Permissions (0-9) =====

    /// Send chat messages (includes messages with position for danmaku display)
    pub const SEND_CHAT: u64 = 1 << 0;

    /// Add movie to playlist
    pub const ADD_MOVIE: u64 = 1 << 1;

    /// Delete own movie
    pub const DELETE_MOVIE_SELF: u64 = 1 << 2;

    /// Delete any movie
    pub const DELETE_MOVIE_ANY: u64 = 1 << 3;

    /// Edit own movie info
    pub const EDIT_MOVIE_SELF: u64 = 1 << 4;

    /// Edit any movie info
    pub const EDIT_MOVIE_ANY: u64 = 1 << 5;

    /// Reorder playlist
    pub const REORDER_PLAYLIST: u64 = 1 << 6;

    /// Clear playlist
    pub const CLEAR_PLAYLIST: u64 = 1 << 7;

    /// Start live stream (RTMP push)
    pub const START_LIVE: u64 = 1 << 8;

    /// Reserved for future content management use (bit 9)
    pub const RESERVED_9: u64 = 1 << 9;

    // ===== Playback Control Permissions (10-19) =====

    /// Play control (play/pause/seek)
    pub const PLAY_CONTROL: u64 = 1 << 10;

    /// Switch current movie
    pub const CHANGE_CURRENT_MOVIE: u64 = 1 << 11;

    /// Change playback rate
    pub const CHANGE_PLAYBACK_RATE: u64 = 1 << 12;

    // ===== Member Management Permissions (20-29) =====

    /// Approve pending members
    pub const APPROVE_MEMBER: u64 = 1 << 20;

    /// Kick member
    pub const KICK_MEMBER: u64 = 1 << 21;

    /// Invite user (alias for `APPROVE_MEMBER`)
    pub const INVITE_USER: u64 = 1 << 20;

    /// Kick user (alias for `KICK_MEMBER`)
    pub const KICK_USER: u64 = 1 << 21;

    /// Ban/unban member
    pub const BAN_MEMBER: u64 = 1 << 22;

    /// Set member permissions
    pub const SET_MEMBER_PERMISSIONS: u64 = 1 << 23;

    /// Grant permissions to members (alias for `SET_MEMBER_PERMISSIONS`)
    pub const GRANT_PERMISSION: u64 = 1 << 23;

    /// Manage admins (promote/demote)
    pub const MANAGE_ADMIN: u64 = 1 << 24;

    // ===== Room Management Permissions (30-39) =====

    /// Modify room settings
    pub const SET_ROOM_SETTINGS: u64 = 1 << 30;

    /// Set room password
    pub const SET_ROOM_PASSWORD: u64 = 1 << 31;

    /// Delete chat messages
    pub const DELETE_CHAT: u64 = 1 << 32;

    /// Delete messages (alias for `DELETE_CHAT`)
    pub const DELETE_MESSAGE: u64 = 1 << 32;

    /// View room statistics
    pub const VIEW_STATS: u64 = 1 << 33;

    /// Export room data
    pub const EXPORT_DATA: u64 = 1 << 34;

    /// Delete room
    pub const DELETE_ROOM: u64 = 1 << 35;

    // ===== Aliases for backward compatibility =====

    /// Update room settings (alias for `SET_ROOM_SETTINGS`)
    pub const UPDATE_ROOM_SETTINGS: u64 = 1 << 30;

    /// Add media (alias for `ADD_MOVIE`)
    pub const ADD_MEDIA: u64 = 1 << 1;

    /// Remove media (alias for `DELETE_MOVIE_ANY`)
    pub const REMOVE_MEDIA: u64 = 1 << 3;

    /// Switch media (alias for `CHANGE_CURRENT_MOVIE`)
    pub const SWITCH_MEDIA: u64 = 1 << 11;

    /// Play/pause (alias for `PLAY_CONTROL`)
    pub const PLAY_PAUSE: u64 = 1 << 10;

    /// Seek (alias for `PLAY_CONTROL`)
    pub const SEEK: u64 = 1 << 10;

    /// Change speed (alias for `CHANGE_PLAYBACK_RATE`)
    pub const CHANGE_SPEED: u64 = 1 << 12;

    /// Revoke permission (alias for `SET_MEMBER_PERMISSIONS`)
    pub const REVOKE_PERMISSION: u64 = 1 << 23;

    // ===== View Permissions (40-49) =====

    /// View playlist
    pub const VIEW_PLAYLIST: u64 = 1 << 40;

    /// View member list
    pub const VIEW_MEMBER_LIST: u64 = 1 << 41;

    /// View chat history
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 42;

    // ===== Communication Permissions (50-59) =====

    /// Use WebRTC (voice/video)
    pub const USE_WEBRTC: u64 = 1 << 50;

    // ===== Reserved (60-63) =====
    // Reserved for future use

    // ===== Permission Combinations =====

    /// All permissions (for Creator)
    pub const ALL: u64 = u64::MAX;

    /// Default member permissions
    pub const DEFAULT_MEMBER: u64 = Self::SEND_CHAT
        | Self::ADD_MOVIE
        | Self::DELETE_MOVIE_SELF
        | Self::EDIT_MOVIE_SELF
        | Self::VIEW_PLAYLIST
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY;

    /// Default admin permissions
    pub const DEFAULT_ADMIN: u64 = Self::DEFAULT_MEMBER
        | Self::DELETE_MOVIE_ANY
        | Self::EDIT_MOVIE_ANY
        | Self::REORDER_PLAYLIST
        | Self::CLEAR_PLAYLIST
        | Self::START_LIVE
        | Self::PLAY_CONTROL
        | Self::CHANGE_CURRENT_MOVIE
        | Self::CHANGE_PLAYBACK_RATE
        | Self::APPROVE_MEMBER
        | Self::KICK_MEMBER
        | Self::BAN_MEMBER
        | Self::SET_MEMBER_PERMISSIONS
        | Self::SET_ROOM_SETTINGS
        | Self::SET_ROOM_PASSWORD
        | Self::DELETE_CHAT
        | Self::VIEW_STATS;

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

    /// Add permissions (alias for grant)
    pub const fn add(&mut self, permission: u64) {
        self.grant(permission);
    }

    /// Remove permissions (alias for revoke)
    pub const fn remove(&mut self, permission: u64) {
        self.revoke(permission);
    }

    /// Toggle permission
    pub const fn toggle(&mut self, permission: u64) {
        self.0 ^= permission;
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
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
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
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
        assert!(!perms.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_permission_grant_revoke() {
        let mut perms = PermissionBits::empty();
        perms.grant(PermissionBits::SEND_CHAT);
        perms.grant(PermissionBits::ADD_MOVIE);

        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));

        perms.revoke(PermissionBits::SEND_CHAT);
        assert!(!perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));
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
        assert!(!guest_perms.has(PermissionBits::ADD_MOVIE));
    }

    #[test]
    fn test_default_member_permissions() {
        let perms = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        assert!(perms.has(PermissionBits::SEND_CHAT));
        assert!(perms.has(PermissionBits::ADD_MOVIE));
        assert!(perms.has(PermissionBits::DELETE_MOVIE_SELF));
        assert!(perms.has(PermissionBits::VIEW_PLAYLIST));
        assert!(!perms.has(PermissionBits::DELETE_MOVIE_ANY));
    }

    #[test]
    fn test_default_admin_permissions() {
        let perms = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        assert!(perms.has_all(PermissionBits::DEFAULT_MEMBER));
        assert!(perms.has(PermissionBits::DELETE_MOVIE_ANY));
        assert!(perms.has(PermissionBits::BAN_MEMBER));
        assert!(perms.has(PermissionBits::SET_ROOM_SETTINGS));
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
        assert!(perms.has(PermissionBits::ADD_MOVIE));
    }

    // ========== Bitmask Operation Edge Cases ==========

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
    fn test_permission_add_is_alias_for_grant() {
        let mut p1 = PermissionBits::empty();
        let mut p2 = PermissionBits::empty();
        p1.grant(PermissionBits::BAN_MEMBER);
        p2.add(PermissionBits::BAN_MEMBER);
        assert_eq!(p1.0, p2.0);
    }

    #[test]
    fn test_permission_remove_is_alias_for_revoke() {
        let mut p1 = PermissionBits(PermissionBits::ALL);
        let mut p2 = PermissionBits(PermissionBits::ALL);
        p1.revoke(PermissionBits::BAN_MEMBER);
        p2.remove(PermissionBits::BAN_MEMBER);
        assert_eq!(p1.0, p2.0);
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
    fn test_all_permissions_bit_coverage() {
        // ALL should be u64::MAX
        assert_eq!(PermissionBits::ALL, u64::MAX);
    }

    #[test]
    fn test_none_permissions_is_zero() {
        assert_eq!(PermissionBits::NONE, 0);
    }

    #[test]
    fn test_default_is_empty() {
        let perms = PermissionBits::default();
        assert_eq!(perms.0, 0);
    }

    #[test]
    fn test_admin_defaults_include_all_member_defaults() {
        // Admin should have a superset of member permissions
        let admin = PermissionBits::DEFAULT_ADMIN;
        let member = PermissionBits::DEFAULT_MEMBER;
        assert_eq!(admin & member, member);
    }

    // ========== Role FromStr / Display ==========

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
