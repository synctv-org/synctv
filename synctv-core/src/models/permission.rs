//! Permission System (Design Document 07-permission-system-design.md)
//!
//! This module implements a database-compatible permission bitmask system.
//!
//! Key features:
//! - Uses `u64` at service/domain boundaries while keeping defined permission bits
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
    SendChatMessages,
    ManageOwnMedia,
    BrowseLibrary,
    ViewMembers,
    ViewChatHistory,
    UseVoiceChat,
    UseP2pMedia,
    DeleteMedia,
    ReorderMedia,
    ClearMedia,
    ManageLiveStreams,
    ControlPlaybackState,
    NavigatePlayback,
    ReviewJoinRequests,
    RemoveMembers,
    ManageMemberPermissions,
    AddMembers,
    ManageRoomSettings,
    DeleteChatMessages,
    DeleteRoom,
    ViewPlaybackHistory,
}

impl RoomPermission {
    pub const SEND_CHAT_MESSAGES: Self = Self::SendChatMessages;
    pub const MANAGE_OWN_MEDIA: Self = Self::ManageOwnMedia;
    pub const BROWSE_LIBRARY: Self = Self::BrowseLibrary;
    pub const VIEW_MEMBERS: Self = Self::ViewMembers;
    pub const VIEW_CHAT_HISTORY: Self = Self::ViewChatHistory;
    pub const USE_VOICE_CHAT: Self = Self::UseVoiceChat;
    pub const USE_P2P_MEDIA: Self = Self::UseP2pMedia;
    pub const DELETE_MEDIA: Self = Self::DeleteMedia;
    pub const REORDER_MEDIA: Self = Self::ReorderMedia;
    pub const CLEAR_MEDIA: Self = Self::ClearMedia;
    pub const MANAGE_LIVE_STREAMS: Self = Self::ManageLiveStreams;
    pub const CONTROL_PLAYBACK_STATE: Self = Self::ControlPlaybackState;
    pub const NAVIGATE_PLAYBACK: Self = Self::NavigatePlayback;
    pub const REVIEW_JOIN_REQUESTS: Self = Self::ReviewJoinRequests;
    pub const REMOVE_MEMBERS: Self = Self::RemoveMembers;
    pub const MANAGE_MEMBER_PERMISSIONS: Self = Self::ManageMemberPermissions;
    pub const ADD_MEMBERS: Self = Self::AddMembers;
    pub const MANAGE_ROOM_SETTINGS: Self = Self::ManageRoomSettings;
    pub const DELETE_CHAT_MESSAGES: Self = Self::DeleteChatMessages;
    pub const DELETE_ROOM: Self = Self::DeleteRoom;
    pub const VIEW_PLAYBACK_HISTORY: Self = Self::ViewPlaybackHistory;
}

/// Effective permissions projected into the admin bitspace for checks and
/// client-visible snapshots. It is derived data, not an override bitspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    pub const SEND_CHAT_MESSAGES: u64 = 1 << 0;
    pub const MANAGE_OWN_MEDIA: u64 = 1 << 1;
    pub const BROWSE_LIBRARY: u64 = 1 << 2;
    pub const VIEW_MEMBERS: u64 = 1 << 3;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 4;
    pub const USE_VOICE_CHAT: u64 = 1 << 5;
    pub const USE_P2P_MEDIA: u64 = 1 << 6;

    pub const ALL: u64 = Self::SEND_CHAT_MESSAGES
        | Self::MANAGE_OWN_MEDIA
        | Self::BROWSE_LIBRARY
        | Self::VIEW_MEMBERS
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_VOICE_CHAT
        | Self::USE_P2P_MEDIA;

    pub const DEFAULT: u64 = Self::ALL;

    pub const NAMES: &[(&str, u64)] = &[
        ("send_chat_messages", Self::SEND_CHAT_MESSAGES),
        ("manage_own_media", Self::MANAGE_OWN_MEDIA),
        ("browse_library", Self::BROWSE_LIBRARY),
        ("view_members", Self::VIEW_MEMBERS),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_voice_chat", Self::USE_VOICE_CHAT),
        ("use_p2p_media", Self::USE_P2P_MEDIA),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::SEND_CHAT_MESSAGES != 0 {
            permissions |= RoomAdminPermissionBits::SEND_CHAT_MESSAGES;
        }
        if bits & Self::MANAGE_OWN_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::MANAGE_OWN_MEDIA;
        }
        if bits & Self::BROWSE_LIBRARY != 0 {
            permissions |= RoomAdminPermissionBits::BROWSE_LIBRARY;
        }
        if bits & Self::VIEW_MEMBERS != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBERS;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_VOICE_CHAT != 0 {
            permissions |= RoomAdminPermissionBits::USE_VOICE_CHAT;
        }
        if bits & Self::USE_P2P_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::USE_P2P_MEDIA;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::SEND_CHAT_MESSAGES != 0 {
            bits |= Self::SEND_CHAT_MESSAGES;
        }
        if permissions & RoomAdminPermissionBits::MANAGE_OWN_MEDIA != 0 {
            bits |= Self::MANAGE_OWN_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::BROWSE_LIBRARY != 0 {
            bits |= Self::BROWSE_LIBRARY;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEMBERS != 0 {
            bits |= Self::VIEW_MEMBERS;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_VOICE_CHAT != 0 {
            bits |= Self::USE_VOICE_CHAT;
        }
        if permissions & RoomAdminPermissionBits::USE_P2P_MEDIA != 0 {
            bits |= Self::USE_P2P_MEDIA;
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
    pub const SEND_CHAT_MESSAGES: u64 = 1 << 0;
    pub const MANAGE_OWN_MEDIA: u64 = 1 << 1;
    pub const BROWSE_LIBRARY: u64 = 1 << 2;
    pub const VIEW_MEMBERS: u64 = 1 << 3;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 4;
    pub const USE_VOICE_CHAT: u64 = 1 << 5;
    pub const DELETE_MEDIA: u64 = 1 << 6;
    pub const REORDER_MEDIA: u64 = 1 << 7;
    pub const CLEAR_MEDIA: u64 = 1 << 8;
    pub const MANAGE_LIVE_STREAMS: u64 = 1 << 9;
    pub const CONTROL_PLAYBACK_STATE: u64 = 1 << 10;
    pub const NAVIGATE_PLAYBACK: u64 = 1 << 11;
    pub const REVIEW_JOIN_REQUESTS: u64 = 1 << 12;
    pub const REMOVE_MEMBERS: u64 = 1 << 13;
    pub const MANAGE_MEMBER_PERMISSIONS: u64 = 1 << 14;
    pub const ADD_MEMBERS: u64 = 1 << 15;
    pub const MANAGE_ROOM_SETTINGS: u64 = 1 << 16;
    pub const DELETE_CHAT_MESSAGES: u64 = 1 << 17;
    pub const DELETE_ROOM: u64 = 1 << 18;
    pub const VIEW_PLAYBACK_HISTORY: u64 = 1 << 19;
    pub const USE_P2P_MEDIA: u64 = 1 << 20;

    pub const ALL: u64 = Self::SEND_CHAT_MESSAGES
        | Self::MANAGE_OWN_MEDIA
        | Self::BROWSE_LIBRARY
        | Self::VIEW_MEMBERS
        | Self::VIEW_CHAT_HISTORY
        | Self::USE_VOICE_CHAT
        | Self::DELETE_MEDIA
        | Self::REORDER_MEDIA
        | Self::CLEAR_MEDIA
        | Self::MANAGE_LIVE_STREAMS
        | Self::CONTROL_PLAYBACK_STATE
        | Self::NAVIGATE_PLAYBACK
        | Self::REVIEW_JOIN_REQUESTS
        | Self::REMOVE_MEMBERS
        | Self::MANAGE_MEMBER_PERMISSIONS
        | Self::ADD_MEMBERS
        | Self::MANAGE_ROOM_SETTINGS
        | Self::DELETE_CHAT_MESSAGES
        | Self::DELETE_ROOM
        | Self::VIEW_PLAYBACK_HISTORY
        | Self::USE_P2P_MEDIA;

    pub const DEFAULT: u64 = Self::ALL & !Self::DELETE_ROOM;

    pub const NAMES: &[(&str, u64)] = &[
        ("send_chat_messages", Self::SEND_CHAT_MESSAGES),
        ("manage_own_media", Self::MANAGE_OWN_MEDIA),
        ("browse_library", Self::BROWSE_LIBRARY),
        ("view_members", Self::VIEW_MEMBERS),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_voice_chat", Self::USE_VOICE_CHAT),
        ("delete_media", Self::DELETE_MEDIA),
        ("reorder_media", Self::REORDER_MEDIA),
        ("clear_media", Self::CLEAR_MEDIA),
        ("manage_live_streams", Self::MANAGE_LIVE_STREAMS),
        ("control_playback_state", Self::CONTROL_PLAYBACK_STATE),
        ("navigate_playback", Self::NAVIGATE_PLAYBACK),
        ("review_join_requests", Self::REVIEW_JOIN_REQUESTS),
        ("remove_members", Self::REMOVE_MEMBERS),
        ("manage_member_permissions", Self::MANAGE_MEMBER_PERMISSIONS),
        ("add_members", Self::ADD_MEMBERS),
        ("manage_room_settings", Self::MANAGE_ROOM_SETTINGS),
        ("delete_chat_messages", Self::DELETE_CHAT_MESSAGES),
        ("delete_room", Self::DELETE_ROOM),
        ("view_playback_history", Self::VIEW_PLAYBACK_HISTORY),
        ("use_p2p_media", Self::USE_P2P_MEDIA),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn bit_for(permission: RoomPermission) -> u64 {
        match permission {
            crate::models::RoomPermission::SendChatMessages => Self::SEND_CHAT_MESSAGES,
            crate::models::RoomPermission::ManageOwnMedia => Self::MANAGE_OWN_MEDIA,
            crate::models::RoomPermission::BrowseLibrary => Self::BROWSE_LIBRARY,
            crate::models::RoomPermission::ViewMembers => Self::VIEW_MEMBERS,
            crate::models::RoomPermission::ViewChatHistory => Self::VIEW_CHAT_HISTORY,
            crate::models::RoomPermission::UseVoiceChat => Self::USE_VOICE_CHAT,
            crate::models::RoomPermission::UseP2pMedia => Self::USE_P2P_MEDIA,
            crate::models::RoomPermission::DeleteMedia => Self::DELETE_MEDIA,
            crate::models::RoomPermission::ReorderMedia => Self::REORDER_MEDIA,
            crate::models::RoomPermission::ClearMedia => Self::CLEAR_MEDIA,
            crate::models::RoomPermission::ManageLiveStreams => Self::MANAGE_LIVE_STREAMS,
            crate::models::RoomPermission::ControlPlaybackState => Self::CONTROL_PLAYBACK_STATE,
            crate::models::RoomPermission::NavigatePlayback => Self::NAVIGATE_PLAYBACK,
            crate::models::RoomPermission::ReviewJoinRequests => Self::REVIEW_JOIN_REQUESTS,
            crate::models::RoomPermission::RemoveMembers => Self::REMOVE_MEMBERS,
            crate::models::RoomPermission::ManageMemberPermissions => {
                Self::MANAGE_MEMBER_PERMISSIONS
            }
            crate::models::RoomPermission::AddMembers => Self::ADD_MEMBERS,
            crate::models::RoomPermission::ManageRoomSettings => Self::MANAGE_ROOM_SETTINGS,
            crate::models::RoomPermission::DeleteChatMessages => Self::DELETE_CHAT_MESSAGES,
            crate::models::RoomPermission::DeleteRoom => Self::DELETE_ROOM,
            crate::models::RoomPermission::ViewPlaybackHistory => Self::VIEW_PLAYBACK_HISTORY,
        }
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::SEND_CHAT_MESSAGES != 0 {
            permissions |= RoomAdminPermissionBits::SEND_CHAT_MESSAGES;
        }
        if bits & Self::MANAGE_OWN_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::MANAGE_OWN_MEDIA;
        }
        if bits & Self::BROWSE_LIBRARY != 0 {
            permissions |= RoomAdminPermissionBits::BROWSE_LIBRARY;
        }
        if bits & Self::VIEW_MEMBERS != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBERS;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_VOICE_CHAT != 0 {
            permissions |= RoomAdminPermissionBits::USE_VOICE_CHAT;
        }
        if bits & Self::DELETE_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_MEDIA;
        }
        if bits & Self::REORDER_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::REORDER_MEDIA;
        }
        if bits & Self::CLEAR_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::CLEAR_MEDIA;
        }
        if bits & Self::MANAGE_LIVE_STREAMS != 0 {
            permissions |= RoomAdminPermissionBits::MANAGE_LIVE_STREAMS;
        }
        if bits & Self::CONTROL_PLAYBACK_STATE != 0 {
            permissions |= RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE;
        }
        if bits & Self::NAVIGATE_PLAYBACK != 0 {
            permissions |= RoomAdminPermissionBits::NAVIGATE_PLAYBACK;
        }
        if bits & Self::REVIEW_JOIN_REQUESTS != 0 {
            permissions |= RoomAdminPermissionBits::REVIEW_JOIN_REQUESTS;
        }
        if bits & Self::REMOVE_MEMBERS != 0 {
            permissions |= RoomAdminPermissionBits::REMOVE_MEMBERS;
        }
        if bits & Self::MANAGE_MEMBER_PERMISSIONS != 0 {
            permissions |= RoomAdminPermissionBits::MANAGE_MEMBER_PERMISSIONS;
        }
        if bits & Self::ADD_MEMBERS != 0 {
            permissions |= RoomAdminPermissionBits::ADD_MEMBERS;
        }
        if bits & Self::MANAGE_ROOM_SETTINGS != 0 {
            permissions |= RoomAdminPermissionBits::MANAGE_ROOM_SETTINGS;
        }
        if bits & Self::DELETE_CHAT_MESSAGES != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_CHAT_MESSAGES;
        }
        if bits & Self::DELETE_ROOM != 0 {
            permissions |= RoomAdminPermissionBits::DELETE_ROOM;
        }
        if bits & Self::VIEW_PLAYBACK_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY;
        }
        if bits & Self::USE_P2P_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::USE_P2P_MEDIA;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::SEND_CHAT_MESSAGES != 0 {
            bits |= Self::SEND_CHAT_MESSAGES;
        }
        if permissions & RoomAdminPermissionBits::MANAGE_OWN_MEDIA != 0 {
            bits |= Self::MANAGE_OWN_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::BROWSE_LIBRARY != 0 {
            bits |= Self::BROWSE_LIBRARY;
        }
        if permissions & RoomAdminPermissionBits::VIEW_MEMBERS != 0 {
            bits |= Self::VIEW_MEMBERS;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_VOICE_CHAT != 0 {
            bits |= Self::USE_VOICE_CHAT;
        }
        if permissions & RoomAdminPermissionBits::DELETE_MEDIA != 0 {
            bits |= Self::DELETE_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::REORDER_MEDIA != 0 {
            bits |= Self::REORDER_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::CLEAR_MEDIA != 0 {
            bits |= Self::CLEAR_MEDIA;
        }
        if permissions & RoomAdminPermissionBits::MANAGE_LIVE_STREAMS != 0 {
            bits |= Self::MANAGE_LIVE_STREAMS;
        }
        if permissions & RoomAdminPermissionBits::CONTROL_PLAYBACK_STATE != 0 {
            bits |= Self::CONTROL_PLAYBACK_STATE;
        }
        if permissions & RoomAdminPermissionBits::NAVIGATE_PLAYBACK != 0 {
            bits |= Self::NAVIGATE_PLAYBACK;
        }
        if permissions & RoomAdminPermissionBits::REVIEW_JOIN_REQUESTS != 0 {
            bits |= Self::REVIEW_JOIN_REQUESTS;
        }
        if permissions & RoomAdminPermissionBits::REMOVE_MEMBERS != 0 {
            bits |= Self::REMOVE_MEMBERS;
        }
        if permissions & RoomAdminPermissionBits::MANAGE_MEMBER_PERMISSIONS != 0 {
            bits |= Self::MANAGE_MEMBER_PERMISSIONS;
        }
        if permissions & RoomAdminPermissionBits::ADD_MEMBERS != 0 {
            bits |= Self::ADD_MEMBERS;
        }
        if permissions & RoomAdminPermissionBits::MANAGE_ROOM_SETTINGS != 0 {
            bits |= Self::MANAGE_ROOM_SETTINGS;
        }
        if permissions & RoomAdminPermissionBits::DELETE_CHAT_MESSAGES != 0 {
            bits |= Self::DELETE_CHAT_MESSAGES;
        }
        if permissions & RoomAdminPermissionBits::DELETE_ROOM != 0 {
            bits |= Self::DELETE_ROOM;
        }
        if permissions & RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY != 0 {
            bits |= Self::VIEW_PLAYBACK_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_P2P_MEDIA != 0 {
            bits |= Self::USE_P2P_MEDIA;
        }
        bits
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomGuestPermissionBits(pub u64);

impl RoomGuestPermissionBits {
    pub const VIEW_MEMBERS: u64 = 1 << 32;
    pub const VIEW_CHAT_HISTORY: u64 = 1 << 33;
    pub const USE_VOICE_CHAT: u64 = 1 << 34;
    pub const USE_P2P_MEDIA: u64 = 1 << 35;

    pub const ALL: u64 =
        Self::VIEW_MEMBERS | Self::VIEW_CHAT_HISTORY | Self::USE_VOICE_CHAT | Self::USE_P2P_MEDIA;
    pub const DEFAULT: u64 = 0;

    pub const NAMES: &[(&str, u64)] = &[
        ("view_members", Self::VIEW_MEMBERS),
        ("view_chat_history", Self::VIEW_CHAT_HISTORY),
        ("use_voice_chat", Self::USE_VOICE_CHAT),
        ("use_p2p_media", Self::USE_P2P_MEDIA),
    ];

    #[must_use]
    pub const fn includes_only_defined(bits: u64) -> bool {
        bits & !Self::ALL == 0
    }

    #[must_use]
    pub const fn to_permissions(bits: u64) -> u64 {
        let mut permissions = 0;
        if bits & Self::VIEW_MEMBERS != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_MEMBERS;
        }
        if bits & Self::VIEW_CHAT_HISTORY != 0 {
            permissions |= RoomAdminPermissionBits::VIEW_CHAT_HISTORY;
        }
        if bits & Self::USE_VOICE_CHAT != 0 {
            permissions |= RoomAdminPermissionBits::USE_VOICE_CHAT;
        }
        if bits & Self::USE_P2P_MEDIA != 0 {
            permissions |= RoomAdminPermissionBits::USE_P2P_MEDIA;
        }
        permissions
    }

    #[must_use]
    pub const fn from_permissions(permissions: u64) -> u64 {
        let mut bits = 0;
        if permissions & RoomAdminPermissionBits::VIEW_MEMBERS != 0 {
            bits |= Self::VIEW_MEMBERS;
        }
        if permissions & RoomAdminPermissionBits::VIEW_CHAT_HISTORY != 0 {
            bits |= Self::VIEW_CHAT_HISTORY;
        }
        if permissions & RoomAdminPermissionBits::USE_VOICE_CHAT != 0 {
            bits |= Self::USE_VOICE_CHAT;
        }
        if permissions & RoomAdminPermissionBits::USE_P2P_MEDIA != 0 {
            bits |= Self::USE_P2P_MEDIA;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl From<Role> for i32 {
    fn from(value: Role) -> Self {
        match value {
            Role::Creator => 1,
            Role::Admin => 2,
            Role::Member => 3,
            Role::Guest => 4,
        }
    }
}

impl TryFrom<i32> for Role {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Creator),
            2 => Ok(Self::Admin),
            3 => Ok(Self::Member),
            4 => Ok(Self::Guest),
            _ => Err(format!("Unknown room member role: {value}")),
        }
    }
}

i16_enum!(Role, "Invalid Role value", {
    Creator = 1,
    Admin = 2,
    Member = 3,
    Guest = 4,
});

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_role(input: &str) -> Role {
        match Role::from_str(input) {
            Ok(role) => role,
            Err(error) => std::panic::panic_any(format!("role should parse: {error}")),
        }
    }

    #[test]
    fn test_permission_has() {
        let perms = RoomPermissionSet::new(RoomAdminPermissionBits::SEND_CHAT_MESSAGES);
        assert!(perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert!(!perms.has(crate::models::RoomPermission::MANAGE_OWN_MEDIA));
    }

    #[test]
    fn test_permission_grant_revoke() {
        let mut perms = RoomPermissionSet::empty();
        perms.grant(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        perms.grant(crate::models::RoomPermission::MANAGE_OWN_MEDIA);

        assert!(perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert!(perms.has(crate::models::RoomPermission::MANAGE_OWN_MEDIA));

        perms.revoke(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(!perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert!(perms.has(crate::models::RoomPermission::MANAGE_OWN_MEDIA));
    }

    #[test]
    fn test_role_permissions() {
        let creator_perms = Role::Creator.permissions();
        assert_eq!(creator_perms.0, RoomAdminPermissionBits::ALL);
        assert!(creator_perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert!(creator_perms.has(crate::models::RoomPermission::DELETE_ROOM));

        let admin_perms = Role::Admin.permissions();
        assert!(!admin_perms.has(crate::models::RoomPermission::DELETE_ROOM));

        let member_perms = Role::Member.permissions();
        assert!(member_perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert!(member_perms.has(crate::models::RoomPermission::USE_VOICE_CHAT));
        assert!(member_perms.has(crate::models::RoomPermission::USE_P2P_MEDIA));
        assert!(!member_perms.has(crate::models::RoomPermission::REMOVE_MEMBERS));

        let guest_perms = Role::Guest.permissions();
        assert!(!guest_perms.has(crate::models::RoomPermission::BROWSE_LIBRARY));
        assert!(!guest_perms.has(crate::models::RoomPermission::MANAGE_OWN_MEDIA));
    }

    #[test]
    fn admin_playback_history_permission_mapping_is_bidirectional() {
        assert_eq!(
            RoomAdminPermissionBits::to_permissions(RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY),
            RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY
        );
        assert_eq!(
            RoomAdminPermissionBits::from_permissions(
                RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY
            ),
            RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY
        );

        let mut member = crate::models::RoomMember::new(
            crate::models::RoomId::expect_positive(1),
            crate::models::UserId::expect_positive(1),
            Role::Admin,
        );
        member.admin_added_permissions = RoomAdminPermissionBits::VIEW_PLAYBACK_HISTORY;
        assert!(member
            .effective_permissions(RoomPermissionSet::empty())
            .has(crate::models::RoomPermission::VIEW_PLAYBACK_HISTORY));
    }

    #[test]
    fn test_allow_deny_pattern() {
        let mut perms = RoomPermissionSet::default_member();

        // Add moderation permission (Allow pattern)
        perms.grant(crate::models::RoomPermission::REMOVE_MEMBERS);
        assert!(perms.has(crate::models::RoomPermission::REMOVE_MEMBERS));

        // Remove chat permission (Deny pattern)
        perms.revoke(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(!perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));

        // Other DEFAULT_MEMBER permissions remain
        assert!(perms.has(crate::models::RoomPermission::MANAGE_OWN_MEDIA));
    }

    #[test]
    fn test_permission_set_enable_disable() {
        let mut perms = RoomPermissionSet::empty();
        perms.set(crate::models::RoomPermission::SEND_CHAT_MESSAGES, true);
        assert!(perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        perms.set(crate::models::RoomPermission::SEND_CHAT_MESSAGES, false);
        assert!(!perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
    }

    #[test]
    fn test_permission_toggle() {
        let mut perms = RoomPermissionSet::empty();
        perms.toggle(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        perms.toggle(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(!perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
    }

    #[test]
    fn test_role_permission_bitspaces_reject_unknown_bits() {
        assert!(RoomMemberPermissionBits::includes_only_defined(
            RoomMemberPermissionBits::SEND_CHAT_MESSAGES
                | RoomMemberPermissionBits::MANAGE_OWN_MEDIA
        ));
        assert!(!RoomMemberPermissionBits::includes_only_defined(1 << 21));
        assert!(RoomAdminPermissionBits::includes_only_defined(
            RoomAdminPermissionBits::REMOVE_MEMBERS
        ));
        assert!(!RoomAdminPermissionBits::includes_only_defined(1 << 21));
    }

    #[test]
    fn test_permission_grant_idempotent() {
        let mut perms = RoomPermissionSet::empty();
        perms.grant(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        perms.grant(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
        assert_eq!(perms.0, RoomAdminPermissionBits::SEND_CHAT_MESSAGES);
    }

    #[test]
    fn test_permission_revoke_idempotent() {
        let mut perms = RoomPermissionSet::empty();
        perms.revoke(crate::models::RoomPermission::SEND_CHAT_MESSAGES);
        assert!(!perms.has(crate::models::RoomPermission::SEND_CHAT_MESSAGES));
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
    fn role_parse_and_display_contract() {
        for (input, role, display) in [
            ("creator", Role::Creator, "creator"),
            ("Admin", Role::Admin, "admin"),
            (" member ", Role::Member, "member"),
            ("GUEST", Role::Guest, "guest"),
        ] {
            assert_eq!(parse_role(input), role);
            assert_eq!(role.to_string(), display);
        }

        assert!(Role::from_str("superadmin").is_err());
        assert!(Role::from_str("").is_err());
        assert!(Role::from_str("owner").is_err());
    }
}
