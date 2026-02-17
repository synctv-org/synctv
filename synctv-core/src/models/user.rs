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
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Root => 1,
            Self::Admin => 2,
            Self::User => 3,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserRole {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
pub enum UserStatus {
    /// Normal active state
    /// - Can login and use all features
    Active,

    /// Pending approval
    /// - Can login but cannot create or join rooms
    Pending,

    /// Banned state
    /// - Cannot login
    /// - All operations forbidden
    Banned,
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
    #[must_use] 
    pub const fn can_login(&self) -> bool {
        matches!(self, Self::Active | Self::Pending)
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
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Active => 1,
            Self::Pending => 2,
            Self::Banned => 3,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserStatus {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignupMethod {
    Email,
    OAuth2,
}

impl SignupMethod {
    #[must_use] 
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::OAuth2 => "oauth2",
        }
    }

    /// Parse signup method from string name (defaults to email for unknown values)
    #[must_use]
    pub fn from_str_name(s: &str) -> Self {
        match s {
            "email" => Self::Email,
            "oauth2" => Self::OAuth2,
            _ => Self::Email, // Default to email
        }
    }
}

// Database mapping: SignupMethod <-> TEXT
impl sqlx::Type<sqlx::Postgres> for SignupMethod {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for SignupMethod {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <&str as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_str(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for SignupMethod {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self::from_str_name(&s))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub email: Option<String>,  // NULL allowed for OAuth2 users
    #[serde(skip_serializing)]
    pub password_hash: String,

    /// User RBAC role (global access level) - SEPARATE from status
    pub role: UserRole,

    /// User status (account state) - SEPARATE from role
    pub status: UserStatus,

    pub signup_method: Option<SignupMethod>,  // NULL for legacy users
    pub email_verified: bool,  // Whether email has been verified
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub password_changed_at: DateTime<Utc>,  // Timestamp of last password change (for token invalidation)
    pub deleted_at: Option<DateTime<Utc>>,
}

impl User {
    #[must_use]
    pub fn new(username: String, email: Option<String>, password_hash: String, signup_method: Option<SignupMethod>) -> Self {
        let now = Utc::now();
        Self {
            id: UserId::new(),
            username,
            email,
            password_hash,
            role: UserRole::User,  // Default role
            status: UserStatus::Pending,  // Default status (requires email verification)
            signup_method,
            email_verified: false,  // Default to not verified
            created_at: now,
            updated_at: now,
            password_changed_at: now,  // Initialize to creation time
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

    /// Check if user can unbind a provider
    /// `OAuth2` users cannot remove all `OAuth2` providers unless they have email
    /// Email users cannot remove their email
    #[must_use] 
    pub const fn can_unbind_provider(&self, has_oauth2_count: usize, has_email: bool) -> bool {
        match self.signup_method {
            None => {
                // Legacy users - allow if they have email or multiple OAuth2
                has_email || has_oauth2_count > 1
            }
            Some(SignupMethod::Email) => {
                // Email users can unbind OAuth2, but need to keep email
                true
            }
            Some(SignupMethod::OAuth2) => {
                // OAuth2 users must keep at least one OAuth2 or add email
                has_oauth2_count > 1 || has_email
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub email: Option<String>,  // Optional email
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    pub username: Option<String>,
    pub email: Option<Option<String>>,  // Option<Option<String>>: Some(None) means set to NULL, None means don't update
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub status: Option<String>, // "active", "banned", etc.
    pub role: Option<String>,   // "user", "admin", "root"
}
