use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::UserId;

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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "root" => Ok(Self::Root),
            "admin" => Ok(Self::Admin),
            "user" => Ok(Self::User),
            _ => Err(format!("Unknown user role: {s}")),
        }
    }
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// Database mapping: UserRole -> SMALLINT (1=root, 2=admin, 3=user)
impl sqlx::Type<sqlx::Postgres> for UserRole {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for UserRole {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Root => 1,
            Self::Admin => 2,
            Self::User => 3,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserRole {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let val = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        match val {
            1 => Ok(Self::Root),
            2 => Ok(Self::Admin),
            3 => Ok(Self::User),
            _ => Err(format!("Invalid UserRole value: {val}").into()),
        }
    }
}

/// User account status (design document 06: role and status separation)
///
/// This represents the user's ACCOUNT state, independent of their role.
/// A user can be Active/Pending/Banned regardless of their Role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(i16)]
pub enum UserStatus {
    /// Normal active state
    /// - Can login and use all features
    Active = 1,

    /// Pending approval
    /// - Can login but cannot create or join rooms
    Pending = 2,

    /// Banned state
    /// - Cannot login
    /// - All operations forbidden
    Banned = 3,
}

impl UserStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Banned => "banned",
        }
    }

    /// Check if user can login with this status
    ///
    /// Only `Active` users can login. `Pending` users must verify their email first (or await
    /// admin approval), and `Banned` users are permanently blocked. This is consistent with the
    /// security pipeline which rejects `Pending` users at token validation time.
    #[must_use]
    pub const fn can_login(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Check if user can create rooms with this status
    #[must_use]
    pub const fn can_create_room(&self) -> bool {
        matches!(self, Self::Active)
    }

    /// Check if user can join rooms with this status
    #[must_use]
    pub const fn can_join_room(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        matches!(self, Self::Banned)
    }
}

impl FromStr for UserStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "pending" => Ok(Self::Pending),
            "banned" => Ok(Self::Banned),
            _ => Err(format!("Unknown user status: {s}")),
        }
    }
}

impl std::fmt::Display for UserStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// Database mapping: UserStatus -> SMALLINT (1=active, 2=pending, 3=banned)
impl sqlx::Type<sqlx::Postgres> for UserStatus {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for UserStatus {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Active => 1,
            Self::Pending => 2,
            Self::Banned => 3,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserStatus {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let val = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        match val {
            1 => Ok(Self::Active),
            2 => Ok(Self::Pending),
            3 => Ok(Self::Banned),
            _ => Err(format!("Invalid UserStatus value: {val}").into()),
        }
    }
}

/// User signup method
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignupMethod {
    /// Unknown or unspecified signup method (default)
    #[default]
    Unknown,
    /// Registered via email verification flow
    Email,
    /// Registered via username + password (no email verification)
    Password,
    /// Registered via OAuth2 provider
    OAuth2,
    /// Created by an administrator
    AdminCreated,
}

impl SignupMethod {
    /// Integer representation for database storage.
    #[must_use]
    pub const fn as_i16(&self) -> i16 {
        match self {
            Self::Unknown => 0,
            Self::Email => 1,
            Self::Password => 2,
            Self::OAuth2 => 3,
            Self::AdminCreated => 4,
        }
    }

    /// Parse from database integer value.
    #[must_use]
    pub const fn from_i16(v: i16) -> Option<Self> {
        match v {
            0 => Some(Self::Unknown),
            1 => Some(Self::Email),
            2 => Some(Self::Password),
            3 => Some(Self::OAuth2),
            4 => Some(Self::AdminCreated),
            _ => None,
        }
    }

    /// String representation for API serialization.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Email => "email",
            Self::Password => "password",
            Self::OAuth2 => "oauth2",
            Self::AdminCreated => "admin_created",
        }
    }

    /// Parse signup method from string name.
    #[must_use]
    pub fn from_str_name(s: &str) -> Option<Self> {
        match s {
            "unknown" => Some(Self::Unknown),
            "email" => Some(Self::Email),
            "password" => Some(Self::Password),
            "oauth2" => Some(Self::OAuth2),
            "admin_created" => Some(Self::AdminCreated),
            _ => None,
        }
    }
}

// Database mapping: SignupMethod <-> SMALLINT
impl sqlx::Type<sqlx::Postgres> for SignupMethod {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for SignupMethod {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_i16(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for SignupMethod {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let v = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::from_i16(v).ok_or_else(|| format!("Unknown SignupMethod value: {v}").into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Option<String>, // NULL allowed for OAuth2 users
    #[serde(skip_serializing)]
    pub password_hash: String,

    /// User RBAC role (global access level) - SEPARATE from status
    pub role: UserRole,

    /// User status (account state) - SEPARATE from role
    pub status: UserStatus,

    pub signup_method: SignupMethod,
    pub email_verified: bool, // Whether email has been verified
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

impl User {
    #[must_use]
    pub fn new(
        username: String,
        email: Option<String>,
        password_hash: String,
        signup_method: SignupMethod,
    ) -> Self {
        Self::new_with_status(
            username,
            email,
            password_hash,
            signup_method,
            UserStatus::Pending,
        )
    }

    /// Create a new user with an explicit initial status.
    ///
    /// Use this when the caller needs to control whether the user starts as `Active`
    /// (email verification disabled) or `Pending` (email verification required).
    #[must_use]
    pub fn new_with_status(
        username: String,
        email: Option<String>,
        password_hash: String,
        signup_method: SignupMethod,
        initial_status: UserStatus,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: UserId::new(),
            username,
            email,
            password_hash,
            role: UserRole::User, // Default role
            status: initial_status,
            signup_method,
            email_verified: false, // Default to not verified
            created_at: now,
            updated_at: now,
            password_changed_at: now, // Initialize to creation time
            password_version: 0,
            version: 0,
            deleted_at: None,
        }
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
        self.status.can_login()
    }

    /// Check if user can create rooms (checks both role and status)
    #[must_use]
    pub const fn can_create_room(&self, allow_user: bool) -> bool {
        if !self.status.can_create_room() {
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
        self.status.can_join_room()
    }

    /// Check if this user has a usable password for authentication.
    ///
    /// A user has usable password auth if:
    /// - They signed up via email or password (explicitly set a password), OR
    /// - They were created by admin and have a password set, OR
    /// - They signed up via `OAuth2` but later set a password (`password_version` > 0 indicates
    ///   the password was explicitly changed after account creation)
    ///
    /// `OAuth2` users initially receive a random password they don't know (`password_version=0`).
    /// If they later use "set password" to establish their own password, `password_version`
    /// increments, indicating they now have a usable password.
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
            SignupMethod::OAuth2 => {
                // OAuth2 users get a random password at signup (pv=0).
                // If pv > 0, the user explicitly changed/set their password.
                self.password_version > 0
            }
        }
    }

    /// Check if user can unbind a provider
    /// `OAuth2` users cannot remove all `OAuth2` providers unless they have email
    /// Other users can unbind freely
    #[must_use]
    pub const fn can_unbind_provider(&self, has_oauth2_count: usize, has_email: bool) -> bool {
        match self.signup_method {
            SignupMethod::Email
            | SignupMethod::Password
            | SignupMethod::AdminCreated
            | SignupMethod::Unknown => true,
            SignupMethod::OAuth2 => {
                // OAuth2 users must keep at least one OAuth2 or add email
                has_oauth2_count > 1 || has_email
            }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub status: Option<UserStatus>,
    pub role: Option<UserRole>,
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
            status: UserStatus::Active,
            signup_method,
            email_verified: true,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version,
            version: 0,
            deleted_at: None,
        }
    }

    // ========================================================================
    // has_usable_password tests
    // ========================================================================

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
        // OAuth2 users start with password_version=0 and a random hash they don't know
        let user = make_test_user(SignupMethod::OAuth2, "$argon2id$random_hash", 0);
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
}
