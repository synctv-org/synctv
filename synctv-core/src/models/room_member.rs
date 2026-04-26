use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::{RoomId, UserId};
use super::permission::{PermissionBits, Role as RoomRole};
use super::query::SortDirection;
use super::room::RoomStatus;

/// Derived member lifecycle.
///
/// The canonical database state is `room_members.left_at`: `NULL` means active,
/// non-`NULL` means the user is no longer an active member. Join approval lives
/// in `room_join_requests`; moderation bans live in `room_member_bans`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MemberStatus {
    #[default]
    Active,
    Left,
}

impl MemberStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Left => "left",
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_left(&self) -> bool {
        matches!(self, Self::Left)
    }
}

impl FromStr for MemberStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "left" => Ok(Self::Left),
            _ => Err(format!("Unknown member status: {s}")),
        }
    }
}

impl std::fmt::Display for MemberStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoomMemberListSortBy {
    Username,
    Role,
    Status,
    #[default]
    JoinedAt,
}

impl RoomMemberListSortBy {
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Username => "u.username",
            Self::Role => "rm.role",
            Self::Status => "rm.left_at",
            Self::JoinedAt => "rm.joined_at",
        }
    }
}

impl std::str::FromStr for RoomMemberListSortBy {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "username" => Ok(Self::Username),
            "role" => Ok(Self::Role),
            "status" => Ok(Self::Status),
            "joined_at" | "joinedat" => Ok(Self::JoinedAt),
            other => Err(format!("Unknown room member list sort field: {other}")),
        }
    }
}

impl std::fmt::Display for RoomMemberListSortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Username => "username",
            Self::Role => "role",
            Self::Status => "status",
            Self::JoinedAt => "joined_at",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMemberListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub role: Option<RoomRole>,
    pub status: Option<MemberStatus>,
    #[serde(default)]
    pub is_banned: Option<bool>,
    pub is_online: Option<bool>,
    #[serde(default)]
    pub sort_by: RoomMemberListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for RoomMemberListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            search: None,
            role: None,
            status: None,
            is_banned: None,
            is_online: None,
            sort_by: RoomMemberListSortBy::JoinedAt,
            sort_direction: SortDirection::Asc,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MyRoomListSortBy {
    Name,
    CreatedAt,
    UpdatedAt,
    LastActivityAt,
    #[default]
    JoinedAt,
}

impl MyRoomListSortBy {
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Name => "r.name",
            Self::CreatedAt => "r.created_at",
            Self::UpdatedAt => "r.updated_at",
            Self::LastActivityAt => "r.last_activity_at",
            Self::JoinedAt => "rm.joined_at",
        }
    }
}

impl std::str::FromStr for MyRoomListSortBy {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "name" => Ok(Self::Name),
            "created_at" | "createdat" => Ok(Self::CreatedAt),
            "updated_at" | "updatedat" => Ok(Self::UpdatedAt),
            "last_activity_at" | "lastactivityat" => Ok(Self::LastActivityAt),
            "joined_at" | "joinedat" => Ok(Self::JoinedAt),
            other => Err(format!("Unknown related room list sort field: {other}")),
        }
    }
}

impl std::fmt::Display for MyRoomListSortBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Name => "name",
            Self::CreatedAt => "created_at",
            Self::UpdatedAt => "updated_at",
            Self::LastActivityAt => "last_activity_at",
            Self::JoinedAt => "joined_at",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MyRoomRelation {
    #[default]
    All,
    Created,
    Participating,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MyRoomListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub status: Option<RoomStatus>,
    #[serde(default)]
    pub is_banned: Option<bool>,
    #[serde(default)]
    pub relation: MyRoomRelation,
    #[serde(default)]
    pub sort_by: MyRoomListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for MyRoomListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            search: None,
            status: None,
            is_banned: None,
            relation: MyRoomRelation::All,
            sort_by: MyRoomListSortBy::JoinedAt,
            sort_direction: SortDirection::Desc,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMember {
    pub room_id: RoomId,
    pub user_id: UserId,

    /// Room role (permission level)
    pub role: RoomRole,
    #[serde(skip)]
    pub status: MemberStatus,

    /// Allow/Deny permission pattern for member role
    /// - `effective_permissions` = (`role_default` | added) & ~removed
    pub added_permissions: u64,
    pub removed_permissions: u64,

    /// Allow/Deny permission pattern for admin role (overrides member-level permissions)
    /// - Only applies when role = Admin
    /// - `effective_permissions` = (`admin_default` | `admin_added`) & ~`admin_removed`
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,

    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,

    /// Version for optimistic locking
    pub version: i64,
    #[serde(skip)]
    pub banned_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub banned_by: Option<UserId>,
    #[serde(skip)]
    pub banned_reason: Option<String>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RoomMember {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        let left_at: Option<DateTime<Utc>> = row.try_get("left_at")?;
        let banned_at = row
            .try_get::<Option<DateTime<Utc>>, _>("banned_at")
            .unwrap_or(None);
        let banned_by = row
            .try_get::<Option<UserId>, _>("banned_by")
            .unwrap_or(None);
        let banned_reason = row
            .try_get::<Option<String>, _>("banned_reason")
            .unwrap_or(None);
        let status = if left_at.is_some() {
            MemberStatus::Left
        } else {
            MemberStatus::Active
        };
        Ok(Self {
            room_id: row.try_get("room_id")?,
            user_id: row.try_get("user_id")?,
            role: row.try_get("role")?,
            status,
            added_permissions: permission_bits_from_row(row, "added_permissions")?,
            removed_permissions: permission_bits_from_row(row, "removed_permissions")?,
            admin_added_permissions: permission_bits_from_row(row, "admin_added_permissions")?,
            admin_removed_permissions: permission_bits_from_row(row, "admin_removed_permissions")?,
            joined_at: row.try_get("joined_at")?,
            left_at,
            version: row.try_get("version")?,
            banned_at,
            banned_by,
            banned_reason,
        })
    }
}

fn permission_bits_from_row(row: &sqlx::postgres::PgRow, column: &str) -> Result<u64, sqlx::Error> {
    use sqlx::Row;

    let bits = row.try_get::<i64, _>(column)?;
    u64::try_from(bits).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

impl RoomMember {
    #[must_use]
    pub fn new(room_id: RoomId, user_id: UserId, role: RoomRole) -> Self {
        let now = Utc::now();
        Self {
            room_id,
            user_id,
            role,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: now,
            left_at: None,
            version: 0,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.left_at.is_none()
    }

    #[must_use]
    pub const fn status(&self) -> MemberStatus {
        if self.left_at.is_some() {
            MemberStatus::Left
        } else {
            MemberStatus::Active
        }
    }

    /// Calculate effective permissions using Allow/Deny pattern
    ///
    /// Permission inheritance chain (three-layer override system):
    /// 1. Global default permissions (from `SettingsRegistry`)
    /// 2. Room-level override: (global | `room_added`) & ~`room_removed`
    /// 3. Member-level override: (`room_level` | `member_added/admin_added`) & ~(`member_removed/admin_removed`)
    ///
    /// Arguments:
    /// - `role_default`: Already-calculated permissions for this role
    ///   (global default with room-level overrides applied)
    ///
    /// This method then applies member-level overrides to get final permissions
    #[must_use]
    pub const fn effective_permissions(&self, role_default: PermissionBits) -> PermissionBits {
        match self.role {
            RoomRole::Creator => {
                // Creator has all permissions (fixed, cannot be modified)
                PermissionBits(PermissionBits::ALL)
            }
            RoomRole::Admin => {
                // Start with role default (already has global + room overrides)
                let mut result = role_default.0;

                // Apply admin-specific Allow/Deny modifications
                result |= self.admin_added_permissions;
                result &= !self.admin_removed_permissions;

                PermissionBits(result)
            }
            RoomRole::Member | RoomRole::Guest => {
                // Start with role default (already has global + room overrides)
                let mut result = role_default.0;

                // Apply member-level Allow/Deny modifications
                result |= self.added_permissions;
                result &= !self.removed_permissions;

                PermissionBits(result)
            }
        }
    }

    /// Check if member has a specific permission (considers both status and effective permissions)
    #[must_use]
    pub const fn has_permission(&self, permission: u64, role_default: PermissionBits) -> bool {
        if self.left_at.is_some()
            || !matches!(self.status, MemberStatus::Active)
            || self.is_banned()
        {
            return false;
        }

        self.effective_permissions(role_default).has(permission)
    }

    pub fn leave(&mut self) {
        self.status = MemberStatus::Left;
        self.left_at = Some(Utc::now());
    }

    pub fn ban(&mut self, banned_by: UserId, reason: Option<String>) {
        let now = Utc::now();
        self.status = MemberStatus::Left;
        self.left_at = Some(now);
        self.banned_at = Some(now);
        self.banned_by = Some(banned_by);
        self.banned_reason = reason;
    }

    pub fn unban(&mut self) {
        self.banned_at = None;
        self.banned_by = None;
        self.banned_reason = None;
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.banned_at.is_some()
    }

    /// Set added permissions (Allow pattern)
    pub const fn add_permissions(&mut self, permissions: u64) {
        self.added_permissions |= permissions;
    }

    /// Set removed permissions (Deny pattern)
    pub const fn remove_permissions(&mut self, permissions: u64) {
        self.removed_permissions |= permissions;
    }

    /// Reset to role default (clear both added and removed)
    pub const fn reset_to_role_default(&mut self) {
        self.added_permissions = 0;
        self.removed_permissions = 0;
        self.admin_added_permissions = 0;
        self.admin_removed_permissions = 0;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMemberWithUser {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub username: String,
    pub role: RoomRole,
    pub status: MemberStatus,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub joined_at: DateTime<Utc>,
    pub left_at: Option<DateTime<Utc>>,
    pub is_online: bool,
    /// Whether the member is still active (has not left the room).
    /// Distinct from `is_online` which tracks WebSocket connection status.
    pub is_active: bool,
    pub is_banned: bool,
    pub banned_at: Option<DateTime<Utc>>,
    pub banned_reason: Option<String>,
}

impl RoomMemberWithUser {
    /// Calculate effective permissions for display
    ///
    /// Arguments:
    /// - `role_default`: Already-calculated permissions for this role
    ///   (global default with room-level overrides applied)
    #[must_use]
    pub fn effective_permissions(&self, role_default: PermissionBits) -> PermissionBits {
        let member = RoomMember {
            room_id: self.room_id.clone(),
            user_id: self.user_id.clone(),
            role: self.role,
            status: self.status,
            added_permissions: self.added_permissions,
            removed_permissions: self.removed_permissions,
            admin_added_permissions: self.admin_added_permissions,
            admin_removed_permissions: self.admin_removed_permissions,
            joined_at: self.joined_at,
            left_at: self.left_at,
            version: 0,
            banned_at: None,
            banned_by: None,
            banned_reason: self.banned_reason.clone(),
        };

        member.effective_permissions(role_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_member(role: RoomRole) -> RoomMember {
        RoomMember::new(
            RoomId("test_room".to_string()),
            UserId("test_user".to_string()),
            role,
        )
    }

    #[test]
    fn test_creator_always_has_all_permissions() {
        let member = test_member(RoomRole::Creator);
        let result = member.effective_permissions(PermissionBits::empty());
        assert_eq!(result.0, PermissionBits::ALL);
    }

    #[test]
    fn test_member_with_added_permissions() {
        let mut member = test_member(RoomRole::Member);
        member.added_permissions = PermissionBits::KICK_MEMBER;
        let default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let result = member.effective_permissions(default);
        assert!(result.has(PermissionBits::KICK_MEMBER));
        assert!(result.has(PermissionBits::SEND_CHAT)); // original kept
    }

    #[test]
    fn test_member_with_removed_permissions() {
        let mut member = test_member(RoomRole::Member);
        member.removed_permissions = PermissionBits::SEND_CHAT;
        let default = PermissionBits(PermissionBits::DEFAULT_MEMBER);
        let result = member.effective_permissions(default);
        assert!(!result.has(PermissionBits::SEND_CHAT));
        assert!(result.has(PermissionBits::ADD_MEDIA)); // other permissions intact
    }

    #[test]
    fn test_admin_uses_admin_overrides() {
        let mut member = test_member(RoomRole::Admin);
        member.admin_added_permissions = PermissionBits::USE_WEBRTC;
        member.admin_removed_permissions = PermissionBits::BAN_MEMBER;
        let default = PermissionBits(PermissionBits::DEFAULT_ADMIN);
        let result = member.effective_permissions(default);
        assert!(result.has(PermissionBits::USE_WEBRTC));
        assert!(!result.has(PermissionBits::BAN_MEMBER));
    }

    #[test]
    fn test_guest_with_added_chat() {
        let mut member = test_member(RoomRole::Guest);
        member.added_permissions = PermissionBits::SEND_CHAT;
        let default = PermissionBits(PermissionBits::DEFAULT_GUEST);
        let result = member.effective_permissions(default);
        assert!(result.has(PermissionBits::SEND_CHAT));
        assert!(result.has(PermissionBits::VIEW_PLAYLIST));
    }

    #[test]
    fn test_member_status() {
        assert!(MemberStatus::Active.is_active());
        assert!(MemberStatus::Left.is_left());
    }

    #[test]
    fn test_member_is_active() {
        let member = test_member(RoomRole::Member);
        assert!(member.is_active());

        let mut left_member = test_member(RoomRole::Member);
        left_member.left_at = Some(Utc::now());
        assert!(!left_member.is_active());

        let mut banned_member = test_member(RoomRole::Member);
        banned_member.ban(UserId("banner".to_string()), None);
        assert!(!banned_member.is_active());
    }

    // ─── C1: ban() must set independent ban state ────────────────────

    #[test]
    fn test_ban_sets_left_at() {
        let mut member = test_member(RoomRole::Member);
        assert!(
            member.left_at.is_none(),
            "Precondition: left_at starts as None"
        );

        let banner = UserId("banner".to_string());
        member.ban(banner.clone(), Some("bad behavior".to_string()));

        assert_eq!(member.status, MemberStatus::Left);
        assert!(
            member.left_at.is_some(),
            "ban() must set left_at because the active member is forced to leave"
        );
        assert!(member.is_banned());
        assert!(member.banned_at.is_some());
        assert_eq!(member.banned_by, Some(banner));
        assert_eq!(member.banned_reason, Some("bad behavior".to_string()));
    }

    #[test]
    fn test_unban_preserves_lifecycle_state() {
        let mut member = test_member(RoomRole::Member);
        let banner = UserId("banner".to_string());
        member.ban(banner, None);
        assert!(member.left_at.is_some(), "Precondition: ban sets left_at");

        member.unban();

        assert_eq!(member.status, MemberStatus::Left);
        assert!(
            member.left_at.is_some(),
            "unban() must not implicitly rejoin a previously banned member"
        );
        assert!(!member.is_banned());
        assert!(member.banned_at.is_none());
        assert!(member.banned_by.is_none());
        assert!(member.banned_reason.is_none());
    }

    // ─── MemberStatus::Left consistency ──────────────────────────────

    #[test]
    fn test_leave_sets_status_and_left_at() {
        let mut member = test_member(RoomRole::Member);
        assert_eq!(
            member.status,
            MemberStatus::Active,
            "Precondition: starts active"
        );
        assert!(
            member.left_at.is_none(),
            "Precondition: left_at starts as None"
        );

        member.leave();

        assert_eq!(
            member.status,
            MemberStatus::Left,
            "leave() must set status to Left"
        );
        assert!(
            member.left_at.is_some(),
            "leave() must set left_at timestamp"
        );
    }

    #[test]
    fn test_room_member_list_sort_by_as_sql() {
        assert_eq!(RoomMemberListSortBy::JoinedAt.as_sql(), "rm.joined_at");
        assert_eq!(RoomMemberListSortBy::Username.as_sql(), "u.username");
        assert_eq!(RoomMemberListSortBy::Role.as_sql(), "rm.role");
    }
}
