use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::UserId;
use super::query::SortDirection;

/// Global user role.
///
/// This represents the user's permission level at the GLOBAL level,
/// independent of their account status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    pub const fn can_manage(&self, other: &Self) -> bool {
        matches!((self, other), (Self::Root, _) | (Self::Admin, Self::User))
    }

    /// Check if this role is admin or above
    #[must_use]
    pub const fn is_admin_or_above(&self) -> bool {
        matches!(self, Self::Root | Self::Admin)
    }

    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Admin => "admin",
            Self::User => "user",
        }
    }
}

impl FromStr for UserRole {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "root" => Ok(Self::Root),
            "admin" => Ok(Self::Admin),
            "user" => Ok(Self::User),
            other => Err(format!("Unknown user role: {other}")),
        }
    }
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<UserRole> for i32 {
    fn from(value: UserRole) -> Self {
        match value {
            UserRole::Root => 1,
            UserRole::Admin => 2,
            UserRole::User => 3,
        }
    }
}

impl TryFrom<i32> for UserRole {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Root),
            2 => Ok(Self::Admin),
            3 => Ok(Self::User),
            _ => Err(format!("Unknown user role: {value}")),
        }
    }
}

/// Effective account status.
///
/// This is not stored on `users`; it is derived from active records in
/// `user_bans`. Registration approval/rejection belongs to
/// `user_registration_requests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[repr(i16)]
pub enum UserStatus {
    #[default]
    Active = 1,
    Banned = 2,
}

impl UserStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Banned => "banned",
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        matches!(self, Self::Banned)
    }

    #[must_use]
    pub const fn can_login(&self) -> bool {
        self.is_active()
    }

    #[must_use]
    pub const fn can_create_room(&self) -> bool {
        self.is_active()
    }

    #[must_use]
    pub const fn can_join_room(&self) -> bool {
        self.is_active()
    }
}

impl FromStr for UserStatus {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "banned" => Ok(Self::Banned),
            other => Err(format!("Unknown user status: {other}")),
        }
    }
}

impl std::fmt::Display for UserStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<UserStatus> for i32 {
    fn from(value: UserStatus) -> Self {
        match value {
            UserStatus::Active => 1,
            UserStatus::Banned => 2,
        }
    }
}

impl TryFrom<i32> for UserStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Active),
            2 => Ok(Self::Banned),
            _ => Err(format!("Unknown user status: {value}")),
        }
    }
}

i16_enum!(UserStatus, "Invalid UserStatus value", {
    Active = 1,
    Banned = 2,
});

i16_enum!(UserRole, "Invalid UserRole value", {
    Root = 1,
    Admin = 2,
    User = 3,
});

/// User signup method
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(i16)]
pub enum SignupMethod {
    /// Unknown or unspecified signup method (default)
    #[default]
    Unknown = 0,
    /// Registered with local email/password credentials
    Email = 1,
    /// Registered via username + password
    Password = 2,
    /// Registered via OAuth2 provider
    OAuth2 = 3,
    /// Created by an administrator
    AdminCreated = 4,
    /// Registered directly with a WebAuthn/passkey credential
    WebAuthn = 5,
}

impl SignupMethod {
    /// Stable string representation for serialized output.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Email => "email",
            Self::Password => "password",
            Self::OAuth2 => "oauth2",
            Self::AdminCreated => "admin_created",
            Self::WebAuthn => "webauthn",
        }
    }

    /// Parse signup method from string name.
    #[must_use]
    pub fn from_str_name(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl FromStr for SignupMethod {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "unknown" => Ok(Self::Unknown),
            "email" => Ok(Self::Email),
            "password" => Ok(Self::Password),
            "oauth2" => Ok(Self::OAuth2),
            "admin_created" => Ok(Self::AdminCreated),
            "webauthn" => Ok(Self::WebAuthn),
            other => Err(format!("Unknown signup method: {other}")),
        }
    }
}

impl std::fmt::Display for SignupMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

i16_enum!(SignupMethod, "Unknown SignupMethod value", {
    Unknown = 0,
    Email = 1,
    Password = 2,
    OAuth2 = 3,
    AdminCreated = 4,
    WebAuthn = 5,
});

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: String,

    /// User RBAC role (global access level) - SEPARATE from status
    pub role: UserRole,
    pub avatar_file_reference_id: Option<i64>,

    #[serde(skip)]
    pub status: UserStatus,

    #[serde(skip)]
    pub is_banned: bool,

    #[serde(skip)]
    pub banned_at: Option<DateTime<Utc>>,

    #[serde(skip)]
    pub banned_by: Option<UserId>,

    #[serde(skip)]
    pub banned_reason: Option<String>,

    pub signup_method: SignupMethod,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Monotonically increasing integer for optimistic locking.
    /// Incremented by `UPDATE … SET version = version + 1 WHERE version = <old>`.
    pub version: i32,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct BlockedUser {
    pub user: User,
    pub blocked_at: DateTime<Utc>,
}

/// Administrative metadata for the account deletion and recovery lifecycle.
///
/// Authentication and normal user lookups only need [`User`]. Keeping this
/// metadata separate prevents lifecycle internals from leaking into public
/// user projections while still giving operators a complete audit view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLifecycleMetadata {
    pub user_id: UserId,
    pub deletion_source: Option<super::DeletionSource>,
    pub deletion_reason: Option<String>,
    pub deleted_by: Option<UserId>,
    pub restored_at: Option<DateTime<Utc>>,
    pub restored_by: Option<UserId>,
}

impl User {
    #[must_use]
    pub fn new(username: String, signup_method: SignupMethod) -> Self {
        let now = crate::SystemClock.now();
        Self {
            id: UserId::new(),
            username,
            role: UserRole::User, // Default role
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method,
            created_at: now,
            updated_at: now,
            version: 0,
            deleted_at: None,
        }
    }

    #[must_use]
    pub fn new_with_status(
        username: String,
        signup_method: SignupMethod,
        initial_status: UserStatus,
    ) -> Self {
        let mut user = Self::new(username, signup_method);
        user.status = initial_status;
        user
    }

    #[must_use]
    pub const fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// Check if user has specific role level (RBAC)
    #[must_use]
    pub const fn is_root(&self) -> bool {
        matches!(self.role, UserRole::Root)
    }

    #[must_use]
    pub const fn is_admin(&self) -> bool {
        matches!(self.role, UserRole::Admin)
    }

    #[must_use]
    pub const fn is_admin_or_above(&self) -> bool {
        self.role.is_admin_or_above()
    }

    /// Check if user can login (checks status, not role)
    #[must_use]
    pub const fn can_login(&self) -> bool {
        self.deleted_at.is_none() && !self.is_banned && self.status.is_active()
    }

    /// Check if user can create rooms (checks both role and status)
    #[must_use]
    pub const fn can_create_room(&self, allow_user: bool) -> bool {
        if self.deleted_at.is_some() || self.is_banned || !self.status.is_active() {
            return false;
        }

        match self.role {
            UserRole::Root | UserRole::Admin => true,
            UserRole::User => allow_user,
        }
    }

    /// Check if user can join rooms (checks status)
    #[must_use]
    pub const fn can_join_room(&self) -> bool {
        self.deleted_at.is_none() && !self.is_banned && self.status.is_active()
    }

    /// Check if user can unbind an OAuth2 provider.
    /// OAuth2 signup users must keep at least one OAuth2 identity.
    /// Other users can unbind freely
    #[must_use]
    pub const fn can_unbind_provider(&self, has_oauth2_count: usize, _has_email: bool) -> bool {
        match self.signup_method {
            SignupMethod::Email
            | SignupMethod::Password
            | SignupMethod::AdminCreated
            | SignupMethod::Unknown
            | SignupMethod::WebAuthn => true,
            SignupMethod::OAuth2 => has_oauth2_count > 1,
        }
    }
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum UserListSortBy {
        Username => { display: "username", sql: "username" },
        Email => { display: "email", sql: "email" },
        Status => { display: "status", sql: "is_banned" },
        Role => { display: "role", sql: "role" },
        UpdatedAt => { display: "updated_at", sql: "updated_at" },
        CreatedAt => { display: "created_at", sql: "created_at" },
    }
    default = CreatedAt;
    error = "Unknown user list sort field";
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    #[serde(default)]
    pub status: Option<UserStatus>,
    pub role: Option<UserRole>,
    #[serde(default)]
    pub is_banned: Option<bool>,
    #[serde(default)]
    pub sort_by: UserListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
    /// Include users in the recovery window. Deleted users remain hidden by default.
    #[serde(default)]
    pub include_deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse<T>(value: &str, context: &str) -> T
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        match value.parse::<T>() {
            Ok(parsed) => parsed,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_signup_method_display_and_parse_roundtrip() {
        assert_eq!(SignupMethod::AdminCreated.to_string(), "admin_created");
        assert_eq!(
            parse::<SignupMethod>("OAUTH2", "oauth2 should parse"),
            SignupMethod::OAuth2
        );
        assert_eq!(
            SignupMethod::from_str_name("admin_created"),
            Some(SignupMethod::AdminCreated)
        );
        assert_eq!(
            parse::<SignupMethod>("webauthn", "webauthn should parse"),
            SignupMethod::WebAuthn
        );
        assert_eq!(SignupMethod::WebAuthn.to_string(), "webauthn");
        assert!("admincreated".parse::<SignupMethod>().is_err());
        assert!("passkey".parse::<SignupMethod>().is_err());
        assert!("ldap".parse::<SignupMethod>().is_err());
    }

    #[test]
    fn test_user_role_and_status_parse_trimmed_case_insensitive_names() {
        assert_eq!(
            parse::<UserRole>(" admin ", "admin role should parse"),
            UserRole::Admin
        );
        assert_eq!(
            parse::<UserRole>(" ROOT ", "root role should parse"),
            UserRole::Root
        );
        assert_eq!(
            parse::<UserStatus>(" banned ", "banned status should parse"),
            UserStatus::Banned
        );
        assert_eq!(
            parse::<UserStatus>(" ACTIVE ", "active status should parse"),
            UserStatus::Active
        );
    }
}
