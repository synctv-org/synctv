use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::UserId;
use super::query::SortDirection;

/// Global user role (design document 06/07: role and status separation)
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

impl From<UserRole> for synctv_proto::common::UserRole {
    fn from(value: UserRole) -> Self {
        match value {
            UserRole::Root => Self::Root,
            UserRole::Admin => Self::Admin,
            UserRole::User => Self::User,
        }
    }
}

impl From<UserRole> for i32 {
    fn from(value: UserRole) -> Self {
        synctv_proto::common::UserRole::from(value) as Self
    }
}

impl TryFrom<synctv_proto::common::UserRole> for UserRole {
    type Error = String;

    fn try_from(value: synctv_proto::common::UserRole) -> Result<Self, Self::Error> {
        match value {
            synctv_proto::common::UserRole::Root => Ok(Self::Root),
            synctv_proto::common::UserRole::Admin => Ok(Self::Admin),
            synctv_proto::common::UserRole::User => Ok(Self::User),
            synctv_proto::common::UserRole::Unspecified => {
                Err(format!("Unknown user role: {}", value as i32))
            }
        }
    }
}

impl TryFrom<i32> for UserRole {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        let proto = synctv_proto::common::UserRole::try_from(value)
            .map_err(|_| format!("Unknown user role: {value}"))?;
        Self::try_from(proto).map_err(|_| format!("Unknown user role: {value}"))
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

impl From<UserStatus> for synctv_proto::common::UserStatus {
    fn from(value: UserStatus) -> Self {
        match value {
            UserStatus::Active => Self::Active,
            UserStatus::Banned => Self::Banned,
        }
    }
}

impl From<UserStatus> for i32 {
    fn from(value: UserStatus) -> Self {
        synctv_proto::common::UserStatus::from(value) as Self
    }
}

impl TryFrom<synctv_proto::common::UserStatus> for UserStatus {
    type Error = String;

    fn try_from(value: synctv_proto::common::UserStatus) -> Result<Self, Self::Error> {
        match value {
            synctv_proto::common::UserStatus::Active => Ok(Self::Active),
            synctv_proto::common::UserStatus::Banned => Ok(Self::Banned),
            synctv_proto::common::UserStatus::Unspecified => {
                Err(format!("Unknown user status: {}", value as i32))
            }
        }
    }
}

impl TryFrom<i32> for UserStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        let proto = synctv_proto::common::UserStatus::try_from(value)
            .map_err(|_| format!("Unknown user status: {value}"))?;
        Self::try_from(proto).map_err(|_| format!("Unknown user status: {value}"))
    }
}

sqlx_i16_enum!(UserRole, "Invalid UserRole value", {
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
    /// String representation for API serialization.
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
            "admin_created" | "admincreated" => Ok(Self::AdminCreated),
            "webauthn" | "passkey" => Ok(Self::WebAuthn),
            other => Err(format!("Unknown signup method: {other}")),
        }
    }
}

impl std::fmt::Display for SignupMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

sqlx_i16_enum!(SignupMethod, "Unknown SignupMethod value", {
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
    pub email: Option<String>, // NULL allowed for OAuth2 users
    #[serde(skip_serializing)]
    pub password_hash: String,

    /// User RBAC role (global access level) - SEPARATE from status
    pub role: UserRole,

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
    pub password_changed_at: DateTime<Utc>, // Timestamp of last password change (for token invalidation)
    /// Monotonically increasing counter, incremented on each password change.
    /// Used to invalidate JWTs via the `pv` claim.
    pub password_version: i32,
    /// Monotonically increasing integer for optimistic locking.
    /// Incremented by `UPDATE … SET version = version + 1 WHERE version = <old>`.
    pub version: i32,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for User {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;

        let is_banned = row.try_get("is_banned").unwrap_or(false);
        let banned_at = row
            .try_get::<Option<DateTime<Utc>>, _>("banned_at")
            .unwrap_or(None);
        let banned_by = row
            .try_get::<Option<UserId>, _>("banned_by")
            .unwrap_or(None);
        let banned_reason = row
            .try_get::<Option<String>, _>("banned_reason")
            .unwrap_or(None);

        Ok(Self {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            email: row.try_get("email")?,
            password_hash: row.try_get("password_hash")?,
            role: row.try_get("role")?,
            status: if is_banned {
                UserStatus::Banned
            } else {
                UserStatus::Active
            },
            is_banned,
            banned_at,
            banned_by,
            banned_reason,
            signup_method: row.try_get("signup_method")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            password_changed_at: row.try_get("password_changed_at")?,
            password_version: row.try_get("password_version")?,
            version: row.try_get("version")?,
            deleted_at: row.try_get("deleted_at")?,
        })
    }
}

impl User {
    #[must_use]
    pub fn new(
        username: String,
        email: Option<String>,
        password_hash: String,
        signup_method: SignupMethod,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: UserId::new(),
            username,
            email,
            password_hash,
            role: UserRole::User, // Default role
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
            signup_method,
            created_at: now,
            updated_at: now,
            password_changed_at: now, // Initialize to creation time
            password_version: 0,
            version: 0,
            deleted_at: None,
        }
    }

    #[must_use]
    pub fn new_with_status(
        username: String,
        email: Option<String>,
        password_hash: String,
        signup_method: SignupMethod,
        initial_status: UserStatus,
    ) -> Self {
        let mut user = Self::new(username, email, password_hash, signup_method);
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

    /// Check if this user has a usable password for authentication.
    ///
    /// A user has usable password auth if:
    /// - They signed up via email or password (explicitly set a password), OR
    /// - They were created by admin and have a password set, OR
    /// - They signed up via `OAuth2` but later set a password (`password_version` > 0 indicates
    ///   password credentials were explicitly added after account creation)
    ///
    /// `OAuth2` users initially have no password credential row. If they later
    /// use "set password", `password_version` increments and the joined password
    /// hash becomes non-empty.
    #[must_use]
    pub const fn has_usable_password(&self) -> bool {
        // Non-empty password hash is a baseline requirement
        if self.password_hash.is_empty() {
            return false;
        }

        match self.signup_method {
            SignupMethod::Email
            | SignupMethod::Password
            | SignupMethod::AdminCreated
            | SignupMethod::Unknown => true,
            SignupMethod::WebAuthn => false,
            SignupMethod::OAuth2 => {
                // OAuth2 users start without password credentials.
                // If pv > 0, the user explicitly changed/set their password.
                self.password_version > 0
            }
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: Option<String>, // Optional email
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    pub email: Option<Option<String>>, // Option<Option<String>>: Some(None) means set to NULL, None means don't update
    pub password: Option<String>,
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum UserListSortBy {
        Username => { display: "username", sql: "username" },
        Email => { display: "email", sql: "email" },
        Status => { display: "status", sql: "is_banned" },
        Role => { display: "role", sql: "role" },
        UpdatedAt => { display: "updated_at", sql: "updated_at", aliases: ["updatedat"] },
        CreatedAt => { display: "created_at", sql: "created_at", aliases: ["createdat"] },
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_user(
        signup_method: SignupMethod,
        password_hash: &str,
        password_version: i32,
    ) -> User {
        let now = Utc::now();
        User {
            id: UserId::new(),
            username: "testuser".to_string(),
            email: Some("test@example.com".to_string()),
            password_hash: password_hash.to_string(),
            role: UserRole::User,
            signup_method,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version,
            version: 0,
            deleted_at: None,
            status: UserStatus::Active,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }

    // has_usable_password tests

    #[test]
    fn test_email_user_has_usable_password() {
        let user = make_test_user(SignupMethod::Email, "$argon2id$fake_hash", 0);
        assert!(
            user.has_usable_password(),
            "Email signup user with non-empty hash should have usable password"
        );
    }

    #[test]
    fn test_email_user_empty_hash_no_usable_password() {
        let user = make_test_user(SignupMethod::Email, "", 0);
        assert!(
            !user.has_usable_password(),
            "Email signup user with empty hash should NOT have usable password"
        );
    }

    #[test]
    fn test_oauth2_user_initial_no_usable_password() {
        // OAuth2 users start with password_version=0 and no password credential.
        let user = make_test_user(SignupMethod::OAuth2, "", 0);
        assert!(
            !user.has_usable_password(),
            "OAuth2 user with pv=0 should NOT have usable password (random password they don't know)"
        );
    }

    #[test]
    fn test_oauth2_user_after_setting_password_has_usable_password() {
        // OAuth2 user who later explicitly set a password (pv > 0)
        let user = make_test_user(SignupMethod::OAuth2, "$argon2id$explicit_hash", 1);
        assert!(
            user.has_usable_password(),
            "OAuth2 user with pv > 0 should have usable password (they explicitly set one)"
        );
    }

    #[test]
    fn test_oauth2_user_empty_hash_no_usable_password() {
        let user = make_test_user(SignupMethod::OAuth2, "", 1);
        assert!(
            !user.has_usable_password(),
            "OAuth2 user with empty hash should NOT have usable password regardless of pv"
        );
    }

    #[test]
    fn test_signup_method_display_and_parse_roundtrip() {
        assert_eq!(SignupMethod::AdminCreated.to_string(), "admin_created");
        assert_eq!(
            "OAUTH2".parse::<SignupMethod>().unwrap(),
            SignupMethod::OAuth2
        );
        assert_eq!(
            SignupMethod::from_str_name("admincreated"),
            Some(SignupMethod::AdminCreated)
        );
        assert_eq!(
            "passkey".parse::<SignupMethod>().unwrap(),
            SignupMethod::WebAuthn
        );
        assert_eq!(SignupMethod::WebAuthn.to_string(), "webauthn");
        assert!("ldap".parse::<SignupMethod>().is_err());
    }

    #[test]
    fn test_user_role_and_status_parse_trimmed_case_insensitive_names() {
        assert_eq!(" admin ".parse::<UserRole>().unwrap(), UserRole::Admin);
        assert_eq!(" ROOT ".parse::<UserRole>().unwrap(), UserRole::Root);
        assert_eq!(
            " banned ".parse::<UserStatus>().unwrap(),
            UserStatus::Banned
        );
        assert_eq!(
            " ACTIVE ".parse::<UserStatus>().unwrap(),
            UserStatus::Active
        );
    }
}
