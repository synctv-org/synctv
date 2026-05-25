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

/// A semantic room permission used by authorization checks.
///
/// This enum deliberately has no numeric representation. Numeric permission
/// bits live in the role-specific bitspaces below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomPermission {
    Chat,
    CreateMediaResource,
    ViewMediaResources,
    ViewMemberList,
    ViewChatHistory,
    UseWebrtc,
    DeleteMediaResourceAny,
    ReorderMediaResources,
    ClearMediaResources,
    LiveControl,
    PlayControl,
    ChangeCurrentMedia,
    ChangePlaybackRate,
    ApproveMember,
    KickMember,
    SetMemberPermissions,
    AddMember,
    SetRoomSettings,
    DeleteChat,
    DeleteRoom,
}

impl RoomPermission {
    pub const CHAT: Self = Self::Chat;
    pub const CREATE_MEDIA_RESOURCE: Self = Self::CreateMediaResource;
    pub const VIEW_MEDIA_RESOURCES: Self = Self::ViewMediaResources;
    pub const VIEW_MEMBER_LIST: Self = Self::ViewMemberList;
    pub const VIEW_CHAT_HISTORY: Self = Self::ViewChatHistory;
    pub const USE_WEBRTC: Self = Self::UseWebrtc;
    pub const DELETE_MEDIA_RESOURCE_ANY: Self = Self::DeleteMediaResourceAny;
    pub const REORDER_MEDIA_RESOURCES: Self = Self::ReorderMediaResources;
    pub const CLEAR_MEDIA_RESOURCES: Self = Self::ClearMediaResources;
    pub const LIVE_CONTROL: Self = Self::LiveControl;
    pub const PLAY_CONTROL: Self = Self::PlayControl;
    pub const CHANGE_CURRENT_MEDIA: Self = Self::ChangeCurrentMedia;
    pub const CHANGE_PLAYBACK_RATE: Self = Self::ChangePlaybackRate;
    pub const APPROVE_MEMBER: Self = Self::ApproveMember;
    pub const KICK_MEMBER: Self = Self::KickMember;
    pub const SET_MEMBER_PERMISSIONS: Self = Self::SetMemberPermissions;
    pub const ADD_MEMBER: Self = Self::AddMember;
    pub const SET_ROOM_SETTINGS: Self = Self::SetRoomSettings;
    pub const DELETE_CHAT: Self = Self::DeleteChat;
    pub const DELETE_ROOM: Self = Self::DeleteRoom;
}

/// Effective permissions projected into the admin bitspace for checks and
/// client-visible snapshots. It is derived data, not an override bitspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomPermissionSet(pub u64);

impl RoomPermissionSet {
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

    #[must_use]
    pub const fn all() -> Self {
        Self(RoomAdminPermissionBits::ALL)
    }

    #[must_use]
    pub const fn default_admin() -> Self {
        Self(RoomAdminPermissionBits::to_permissions(
            RoomAdminPermissionBits::DEFAULT,
        ))
    }

    #[must_use]
    pub const fn default_member() -> Self {
        Self(RoomMemberPermissionBits::to_permissions(
            RoomMemberPermissionBits::DEFAULT,
        ))
    }

    #[must_use]
    pub const fn default_guest() -> Self {
        Self(RoomGuestPermissionBits::to_permissions(
            RoomGuestPermissionBits::DEFAULT,
        ))
    }

    #[must_use]
    pub const fn guest_assignable() -> Self {
        Self(RoomGuestPermissionBits::to_permissions(
            RoomGuestPermissionBits::ALL,
        ))
    }

    #[must_use]
    pub const fn has(&self, permission: RoomPermission) -> bool {
        let bit = RoomAdminPermissionBits::bit_for(permission);
        bit != 0 && (self.0 & bit) != 0
    }

    #[must_use]
    pub const fn has_all(&self, permissions: Self) -> bool {
        (self.0 & permissions.0) == permissions.0
    }

    #[must_use]
    pub const fn has_any(&self, permissions: Self) -> bool {
        (self.0 & permissions.0) != 0
    }

    pub const fn grant(&mut self, permission: RoomPermission) {
        self.0 |= RoomAdminPermissionBits::bit_for(permission);
    }

    pub const fn revoke(&mut self, permission: RoomPermission) {
        self.0 &= !RoomAdminPermissionBits::bit_for(permission);
    }

    pub const fn set(&mut self, permission: RoomPermission, enabled: bool) {
        if enabled {
            self.grant(permission);
        } else {
            self.revoke(permission);
        }
    }

    pub const fn toggle(&mut self, permission: RoomPermission) {
        self.0 ^= RoomAdminPermissionBits::bit_for(permission);
    }
}

/// Permission bitspace for non-admin room members.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomMemberPermissionBits(pub u64);

impl RoomMemberPermissionBits {
    pub const CHAT: u64 = 1 << 0;
    pub const CREATE_MEDIA_RESOURCE: u64 = 1 << 1;
    pub const VIEW_MEDIA_RESOURCES: u64 = 1 << 2;
    pub const VIEW_MEMBER_LIST: u64 = 1 << 3;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 4;
    pub const USE_WEBRTC: u64 = 1 << 5;

    pub const ALL: u64 = Self::CHAT
        | Self::CREATE_MEDIA_RESOURCE
        | Self::VIEW_MEDIA_RESOURCES
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_WEBRTC;

    pub const DEFAULT: u64 = Self::ALL;

    pub const NAMES: &[(&str, u64)] = &[
        ("chat", Self::CHAT),
        ("create_media_resource", Self::CREATE_MEDIA_RESOURCE),
        ("view_media_resources", Self::VIEW_MEDIA_RESOURCES),
        ("view_member_list", Self::VIEW_MEMBER_LIST),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_webrtc", Self::USE_WEBRTC),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::CHAT != 0 {
            permissions |= RoomAdminPermissionBits::CHAT;
        }
        if bits & Self::CREATE_MEDIA_RESOURCE != 0 {
            permissions |= RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE;
        }
        if bits & Self::VIEW_MEDIA_RESOURCES != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES;
        }
        if bits & Self::VIEW_MEMBER_LIST != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBER_LIST;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_WEBRTC != 0 {
            permissions |= RoomAdminPermissionBits::USE_WEBRTC;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::CHAT != 0 {
            bits |= Self::CHAT;
        }
        if permissions & RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE != 0 {
            bits |= Self::CREATE_MEDIA_RESOURCE;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES != 0 {
            bits |= Self::VIEW_MEDIA_RESOURCES;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEMBER_LIST != 0 {
            bits |= Self::VIEW_MEMBER_LIST;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_WEBRTC != 0 {
            bits |= Self::USE_WEBRTC;
        }
        bits
    }
}

/// Permission bitspace for room admins.
///
/// Admin bits are independent from member bits even when some names map to the
/// same runtime capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomAdminPermissionBits(pub u64);

impl RoomAdminPermissionBits {
    pub const CHAT: u64 = 1 << 0;
    pub const CREATE_MEDIA_RESOURCE: u64 = 1 << 1;
    pub const VIEW_MEDIA_RESOURCES: u64 = 1 << 2;
    pub const VIEW_MEMBER_LIST: u64 = 1 << 3;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 4;
    pub const USE_WEBRTC: u64 = 1 << 5;
    pub const DELETE_MEDIA_RESOURCE_ANY: u64 = 1 << 6;
    pub const REORDER_MEDIA_RESOURCES: u64 = 1 << 7;
    pub const CLEAR_MEDIA_RESOURCES: u64 = 1 << 8;
    pub const LIVE_CONTROL: u64 = 1 << 9;
    pub const PLAY_CONTROL: u64 = 1 << 10;
    pub const CHANGE_CURRENT_MEDIA: u64 = 1 << 11;
    pub const CHANGE_PLAYBACK_RATE: u64 = 1 << 12;
    pub const APPROVE_MEMBER: u64 = 1 << 13;
    pub const KICK_MEMBER: u64 = 1 << 14;
    pub const SET_MEMBER_PERMISSIONS: u64 = 1 << 15;
    pub const ADD_MEMBER: u64 = 1 << 16;
    pub const SET_ROOM_SETTINGS: u64 = 1 << 17;
    pub const DELETE_CHAT: u64 = 1 << 18;
    pub const DELETE_ROOM: u64 = 1 << 19;

    pub const ALL: u64 = Self::CHAT
        | Self::CREATE_MEDIA_RESOURCE
        | Self::VIEW_MEDIA_RESOURCES
        | Self::VIEW_MEMBER_LIST
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_WEBRTC
        | Self::DELETE_MEDIA_RESOURCE_ANY
        | Self::REORDER_MEDIA_RESOURCES
        | Self::CLEAR_MEDIA_RESOURCES
        | Self::LIVE_CONTROL
        | Self::PLAY_CONTROL
        | Self::CHANGE_CURRENT_MEDIA
        | Self::CHANGE_PLAYBACK_RATE
        | Self::APPROVE_MEMBER
        | Self::KICK_MEMBER
        | Self::SET_MEMBER_PERMISSIONS
        | Self::ADD_MEMBER
        | Self::SET_ROOM_SETTINGS
        | Self::DELETE_CHAT
        | Self::DELETE_ROOM;

    pub const DEFAULT: u64 = Self::ALL & !Self::DELETE_ROOM;

    pub const NAMES: &[(&str, u64)] = &[
        ("chat", Self::CHAT),
        ("create_media_resource", Self::CREATE_MEDIA_RESOURCE),
        ("view_media_resources", Self::VIEW_MEDIA_RESOURCES),
        ("view_member_list", Self::VIEW_MEMBER_LIST),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_webrtc", Self::USE_WEBRTC),
        ("delete_media_resource_any", Self::DELETE_MEDIA_RESOURCE_ANY),
        ("reorder_media_resources", Self::REORDER_MEDIA_RESOURCES),
        ("clear_media_resources", Self::CLEAR_MEDIA_RESOURCES),
        ("live_control", Self::LIVE_CONTROL),
        ("play_control", Self::PLAY_CONTROL),
        ("change_current_media", Self::CHANGE_CURRENT_MEDIA),
        ("change_playback_rate", Self::CHANGE_PLAYBACK_RATE),
        ("approve_member", Self::APPROVE_MEMBER),
        ("kick_member", Self::KICK_MEMBER),
        ("set_member_permissions", Self::SET_MEMBER_PERMISSIONS),
        ("add_member", Self::ADD_MEMBER),
        ("set_room_settings", Self::SET_ROOM_SETTINGS),
        ("delete_chat", Self::DELETE_CHAT),
        ("delete_room", Self::DELETE_ROOM),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn bit_for(permission: RoomPermission) -> u64 {
        match permission {
            crate::models::RoomPermission::Chat => Self::CHAT,
            crate::models::RoomPermission::CreateMediaResource => Self::CREATE_MEDIA_RESOURCE,
            crate::models::RoomPermission::ViewMediaResources => Self::VIEW_MEDIA_RESOURCES,
            crate::models::RoomPermission::ViewMemberList => Self::VIEW_MEMBER_LIST,
            crate::models::RoomPermission::ViewChatHistory => Self::VIEW_CHAT_HISTORY,
            crate::models::RoomPermission::UseWebrtc => Self::USE_WEBRTC,
            crate::models::RoomPermission::DeleteMediaResourceAny => {
                Self::DELETE_MEDIA_RESOURCE_ANY
            }
            crate::models::RoomPermission::ReorderMediaResources => Self::REORDER_MEDIA_RESOURCES,
            crate::models::RoomPermission::ClearMediaResources => Self::CLEAR_MEDIA_RESOURCES,
            crate::models::RoomPermission::LiveControl => Self::LIVE_CONTROL,
            crate::models::RoomPermission::PlayControl => Self::PLAY_CONTROL,
            crate::models::RoomPermission::ChangeCurrentMedia => Self::CHANGE_CURRENT_MEDIA,
            crate::models::RoomPermission::ChangePlaybackRate => Self::CHANGE_PLAYBACK_RATE,
            crate::models::RoomPermission::ApproveMember => Self::APPROVE_MEMBER,
            crate::models::RoomPermission::KickMember => Self::KICK_MEMBER,
            crate::models::RoomPermission::SetMemberPermissions => Self::SET_MEMBER_PERMISSIONS,
            crate::models::RoomPermission::AddMember => Self::ADD_MEMBER,
            crate::models::RoomPermission::SetRoomSettings => Self::SET_ROOM_SETTINGS,
            crate::models::RoomPermission::DeleteChat => Self::DELETE_CHAT,
            crate::models::RoomPermission::DeleteRoom => Self::DELETE_ROOM,
        }
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::CHAT != 0 {
            permissions |= RoomAdminPermissionBits::CHAT;
        }
        if bits & Self::CREATE_MEDIA_RESOURCE != 0 {
            permissions |= RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE;
        }
        if bits & Self::VIEW_MEDIA_RESOURCES != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES;
        }
        if bits & Self::VIEW_MEMBER_LIST != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBER_LIST;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_WEBRTC != 0 {
            permissions |= RoomAdminPermissionBits::USE_WEBRTC;
        }
        if bits & Self::DELETE_MEDIA_RESOURCE_ANY != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY;
        }
        if bits & Self::REORDER_MEDIA_RESOURCES != 0 {
            permissions |= RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES;
        }
        if bits & Self::CLEAR_MEDIA_RESOURCES != 0 {
            permissions |= RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES;
        }
        if bits & Self::LIVE_CONTROL != 0 {
            permissions |= RoomAdminPermissionBits::LIVE_CONTROL;
        }
        if bits & Self::PLAY_CONTROL != 0 {
            permissions |= RoomAdminPermissionBits::PLAY_CONTROL;
        }
        if bits & Self::CHANGE_CURRENT_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::CHANGE_CURRENT_MEDIA;
        }
        if bits & Self::CHANGE_PLAYBACK_RATE != 0 {
            permissions |= RoomAdminPermissionBits::CHANGE_PLAYBACK_RATE;
        }
        if bits & Self::APPROVE_MEMBER != 0 {
            permissions |= RoomAdminPermissionBits::APPROVE_MEMBER;
        }
        if bits & Self::KICK_MEMBER != 0 {
            permissions |= RoomAdminPermissionBits::KICK_MEMBER;
        }
        if bits & Self::SET_MEMBER_PERMISSIONS != 0 {
            permissions |= RoomAdminPermissionBits::SET_MEMBER_PERMISSIONS;
        }
        if bits & Self::ADD_MEMBER != 0 {
            permissions |= RoomAdminPermissionBits::ADD_MEMBER;
        }
        if bits & Self::SET_ROOM_SETTINGS != 0 {
            permissions |= RoomAdminPermissionBits::SET_ROOM_SETTINGS;
        }
        if bits & Self::DELETE_CHAT != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_CHAT;
        }
        if bits & Self::DELETE_ROOM != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_ROOM;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::CHAT != 0 {
            bits |= Self::CHAT;
        }
        if permissions & RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE != 0 {
            bits |= Self::CREATE_MEDIA_RESOURCE;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEDIA_RESOURCES != 0 {
            bits |= Self::VIEW_MEDIA_RESOURCES;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEMBER_LIST != 0 {
            bits |= Self::VIEW_MEMBER_LIST;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_WEBRTC != 0 {
            bits |= Self::USE_WEBRTC;
        }
        if permissions & RoomAdminPermissionBits::DELETE_MEDIA_RESOURCE_ANY != 0 {
            bits |= Self::DELETE_MEDIA_RESOURCE_ANY;
        }
        if permissions & RoomAdminPermissionBits::REORDER_MEDIA_RESOURCES != 0 {
            bits |= Self::REORDER_MEDIA_RESOURCES;
        }
        if permissions & RoomAdminPermissionBits::CLEAR_MEDIA_RESOURCES != 0 {
            bits |= Self::CLEAR_MEDIA_RESOURCES;
        }
        if permissions & RoomAdminPermissionBits::LIVE_CONTROL != 0 {
            bits |= Self::LIVE_CONTROL;
        }
        if permissions & RoomAdminPermissionBits::PLAY_CONTROL != 0 {
            bits |= Self::PLAY_CONTROL;
        }
        if permissions & RoomAdminPermissionBits::CHANGE_CURRENT_MEDIA != 0 {
            bits |= Self::CHANGE_CURRENT_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::CHANGE_PLAYBACK_RATE != 0 {
            bits |= Self::CHANGE_PLAYBACK_RATE;
        }
        if permissions & RoomAdminPermissionBits::APPROVE_MEMBER != 0 {
            bits |= Self::APPROVE_MEMBER;
        }
        if permissions & RoomAdminPermissionBits::KICK_MEMBER != 0 {
            bits |= Self::KICK_MEMBER;
        }
        if permissions & RoomAdminPermissionBits::SET_MEMBER_PERMISSIONS != 0 {
            bits |= Self::SET_MEMBER_PERMISSIONS;
        }
        if permissions & RoomAdminPermissionBits::ADD_MEMBER != 0 {
            bits |= Self::ADD_MEMBER;
        }
        if permissions & RoomAdminPermissionBits::SET_ROOM_SETTINGS != 0 {
            bits |= Self::SET_ROOM_SETTINGS;
        }
        if permissions & RoomAdminPermissionBits::DELETE_CHAT != 0 {
            bits |= Self::DELETE_CHAT;
        }
        if permissions & RoomAdminPermissionBits::DELETE_ROOM != 0 {
            bits |= Self::DELETE_ROOM;
        }
        bits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomGuestPermissionBits(pub u64);

impl RoomGuestPermissionBits {
    pub const VIEW_MEMBER_LIST: u64 = 1 << 32;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 33;
    pub const USE_WEBRTC: u64 = 1 << 34;

    pub const ALL: u64 = Self::VIEW_MEMBER_LIST | Self::VIEW_CHAT_HISTORY | Self::USE_WEBRTC;
    pub const DEFAULT: u64 = 0;

    pub const NAMES: &[(&str, u64)] = &[
        ("view_member_list", Self::VIEW_MEMBER_LIST),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_webrtc", Self::USE_WEBRTC),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::VIEW_MEMBER_LIST != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBER_LIST;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_WEBRTC != 0 {
            permissions |= RoomAdminPermissionBits::USE_WEBRTC;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::VIEW_MEMBER_LIST != 0 {
            bits |= Self::VIEW_MEMBER_LIST;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_WEBRTC != 0 {
            bits |= Self::USE_WEBRTC;
        }
        bits
    }
}

impl Default for RoomPermissionSet {
    fn default() -> Self {
        Self::empty()
    }
}

impl From<u64> for RoomPermissionSet {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<RoomPermissionSet> for u64 {
    fn from(value: RoomPermissionSet) -> Self {
        value.0
    }
}

impl BitOr for RoomPermissionSet {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOr<u64> for RoomPermissionSet {
    type Output = Self;

    fn bitor(self, rhs: u64) -> Self::Output {
        Self(self.0 | rhs)
    }
}

impl BitOrAssign for RoomPermissionSet {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitOrAssign<u64> for RoomPermissionSet {
    fn bitor_assign(&mut self, rhs: u64) {
        self.0 |= rhs;
    }
}

impl BitAnd for RoomPermissionSet {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAnd<u64> for RoomPermissionSet {
    type Output = Self;

    fn bitand(self, rhs: u64) -> Self::Output {
        Self(self.0 & rhs)
    }
}

impl BitAndAssign for RoomPermissionSet {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitAndAssign<u64> for RoomPermissionSet {
    fn bitand_assign(&mut self, rhs: u64) {
        self.0 &= rhs;
    }
}

impl BitXor for RoomPermissionSet {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        Self(self.0 ^ rhs.0)
    }
}

impl BitXor<u64> for RoomPermissionSet {
    type Output = Self;

    fn bitxor(self, rhs: u64) -> Self::Output {
        Self(self.0 ^ rhs)
    }
}

impl BitXorAssign for RoomPermissionSet {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

impl BitXorAssign<u64> for RoomPermissionSet {
    fn bitxor_assign(&mut self, rhs: u64) {
        self.0 ^= rhs;
    }
}

impl Not for RoomPermissionSet {
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
    pub const fn permissions(&self) -> RoomPermissionSet {
        match self {
            Self::Creator => RoomPermissionSet::all(),
            Self::Admin => RoomPermissionSet::default_admin(),
            Self::Member => RoomPermissionSet::default_member(),
            Self::Guest => RoomPermissionSet::default_guest(),
        }
    }

    #[must_use]
    pub const fn override_bits_from_permissions(&self, permissions: u64) -> u64 {
        match self {
            Self::Creator | Self::Admin => RoomAdminPermissionBits::from_permissions(permissions),
            Self::Member => RoomMemberPermissionBits::from_permissions(permissions),
            Self::Guest => RoomGuestPermissionBits::from_permissions(permissions),
        }
    }

    #[must_use]
    pub const fn permissions_from_override_bits(&self, bits: u64) -> u64 {
        match self {
            Self::Creator | Self::Admin => RoomAdminPermissionBits::to_permissions(bits),
            Self::Member => RoomMemberPermissionBits::to_permissions(bits),
            Self::Guest => RoomGuestPermissionBits::to_permissions(bits),
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
        let perms = RoomPermissionSet::new(RoomAdminPermissionBits::CHAT);
        assert!(perms.has(crate::models::RoomPermission::CHAT));
        assert!(!perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_grant_revoke() {
        let mut perms = RoomPermissionSet::empty();
        perms.grant(crate::models::RoomPermission::CHAT);
        perms.grant(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE);

        assert!(perms.has(crate::models::RoomPermission::CHAT));
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));

        perms.revoke(crate::models::RoomPermission::CHAT);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_role_permissions() {
        let creator_perms = Role::Creator.permissions();
        assert_eq!(creator_perms.0, RoomAdminPermissionBits::ALL);
        assert!(creator_perms.has(crate::models::RoomPermission::CHAT));
        assert!(creator_perms.has(crate::models::RoomPermission::DELETE_ROOM));

        let admin_perms = Role::Admin.permissions();
        assert!(!admin_perms.has(crate::models::RoomPermission::DELETE_ROOM));

        let member_perms = Role::Member.permissions();
        assert!(member_perms.has(crate::models::RoomPermission::CHAT));
        assert!(member_perms.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(!member_perms.has(crate::models::RoomPermission::KICK_MEMBER));

        let guest_perms = Role::Guest.permissions();
        assert!(!guest_perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
        assert!(!guest_perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_allow_deny_pattern() {
        let mut perms = RoomPermissionSet::default_member();

        // Add moderation permission (Allow pattern)
        perms.grant(crate::models::RoomPermission::KICK_MEMBER);
        assert!(perms.has(crate::models::RoomPermission::KICK_MEMBER));

        // Remove chat permission (Deny pattern)
        perms.revoke(crate::models::RoomPermission::CHAT);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));

        // Other DEFAULT_MEMBER permissions remain
        assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_permission_set_enable_disable() {
        let mut perms = RoomPermissionSet::empty();
        perms.set(crate::models::RoomPermission::CHAT, true);
        assert!(perms.has(crate::models::RoomPermission::CHAT));
        perms.set(crate::models::RoomPermission::CHAT, false);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_permission_toggle() {
        let mut perms = RoomPermissionSet::empty();
        perms.toggle(crate::models::RoomPermission::CHAT);
        assert!(perms.has(crate::models::RoomPermission::CHAT));
        perms.toggle(crate::models::RoomPermission::CHAT);
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_permission_bits_std_bit_ops() {
        let chat = RoomPermissionSet::new(RoomAdminPermissionBits::CHAT);
        let media = RoomPermissionSet::new(RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE);
        let combined = chat | media;

        assert!(combined.has_all(RoomPermissionSet::new(
            RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
        )));
        assert_eq!((combined & chat).bits(), RoomAdminPermissionBits::CHAT);
        assert_eq!(
            (combined ^ chat).bits(),
            RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
        );

        let mut assigned = RoomPermissionSet::empty();
        assigned |= RoomAdminPermissionBits::CHAT;
        assigned |= media;
        assigned &= !chat;
        assert_eq!(
            assigned.bits(),
            RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
        );
    }

    #[test]
    fn test_role_permission_bitspaces_reject_unknown_bits() {
        assert!(RoomMemberPermissionBits::includes_only_defined(
            RoomMemberPermissionBits::CHAT | RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE
        ));
        assert!(!RoomMemberPermissionBits::includes_only_defined(1 << 21));
        assert!(RoomAdminPermissionBits::includes_only_defined(
            RoomAdminPermissionBits::KICK_MEMBER
        ));
        assert!(!RoomAdminPermissionBits::includes_only_defined(1 << 21));
    }

    #[test]
    fn test_permission_grant_idempotent() {
        let mut perms = RoomPermissionSet::empty();
        perms.grant(crate::models::RoomPermission::CHAT);
        perms.grant(crate::models::RoomPermission::CHAT);
        assert!(perms.has(crate::models::RoomPermission::CHAT));
        assert_eq!(perms.0, RoomAdminPermissionBits::CHAT);
    }

    #[test]
    fn test_permission_revoke_idempotent() {
        let mut perms = RoomPermissionSet::empty();
        perms.revoke(crate::models::RoomPermission::CHAT); // no-op on empty
        assert!(!perms.has(crate::models::RoomPermission::CHAT));
        assert_eq!(perms.0, 0);
    }

    #[test]
    fn test_has_all_with_zero_is_vacuously_true() {
        let perms = RoomPermissionSet::empty();
        assert!(perms.has_all(RoomPermissionSet::empty()));
    }

    #[test]
    fn test_has_any_with_zero_is_false() {
        let perms = RoomPermissionSet::all();
        assert!(!perms.has_any(RoomPermissionSet::empty()));
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
