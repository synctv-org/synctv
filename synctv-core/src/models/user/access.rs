#[cfg(verus_only)]
use vstd::prelude::*;

/// Global user role.
///
/// This represents the user's permission level at the GLOBAL level,
/// independent of their account status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(verus_only),
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(verus_only, verus_verify)]
pub enum UserRole {
    /// Root user (super administrator)
    /// - Can manage all admins
    /// - Can access all rooms
    /// - Can modify global settings
    Root,

    /// Platform administrator
    /// - Can manage regular users (approve, ban, delete)
    /// - Can manage rooms (approve, ban, delete)
    /// - Cannot manage Root users
    Admin,

    /// Regular user
    /// - Can create rooms (subject to global config)
    /// - Can join rooms
    User,
}

impl UserRole {
    /// Check if this role can manage another role
    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Root || (self is Admin && other is User))))]
    pub const fn can_manage(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Root, _) | (Self::Admin, Self::User))
    }

    /// Check if this role is admin or above
    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == !(self is User)))]
    pub const fn is_admin_or_above(&self) -> bool {
        matches!(self, Self::Root | Self::Admin)
    }
}

/// Effective account status.
///
/// This is not stored on `users`; it is derived from active records in
/// `user_bans`. Registration approval/rejection belongs to
/// `user_registration_requests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(
    not(verus_only),
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "lowercase")
)]
#[cfg_attr(verus_only, verus_verify)]
#[repr(i16)]
pub enum UserStatus {
    #[default]
    Active = 1,
    Banned = 2,
}

impl UserStatus {
    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Active)))]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Banned)))]
    pub const fn is_banned(&self) -> bool {
        matches!(self, Self::Banned)
    }

    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Active)))]
    pub const fn can_login(&self) -> bool {
        self.is_active()
    }

    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Active)))]
    pub const fn can_create_room(&self) -> bool {
        self.is_active()
    }

    #[must_use]
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Active)))]
    pub const fn can_join_room(&self) -> bool {
        self.is_active()
    }
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == (self is Active && !banned && !deleted)))]
    pub(crate) const fn permits_session(self, banned: bool, deleted: bool) -> bool {
        self.is_active() && !banned && !deleted
    }

    #[cfg_attr(verus_only, verus_spec(ok => ensures ok ==
        (self is Active && !banned && !deleted && (!(role is User) || allow_user))))]
    pub(super) const fn permits_room_creation(
        self,
        banned: bool,
        deleted: bool,
        role: UserRole,
        allow_user: bool,
    ) -> bool {
        self.permits_session(banned, deleted) && (role.is_admin_or_above() || allow_user)
    }
}
