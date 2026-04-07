use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::{RoomId, UserId};
use super::permission::{PermissionBits, Role as RoomRole};
use super::query::SortDirection;
use super::room::RoomStatus;

/// Member status in room (independent of role)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MemberStatus {
    /// Active member
    #[default]
    Active,
    /// Pending approval (if room requires approval)
    Pending,
    /// Join request or invitation was explicitly rejected
    Rejected,
    /// Banned from room
    Banned,
    /// Left the room (soft-deleted membership)
    Left,
}

impl MemberStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Rejected => "rejected",
            Self::Banned => "banned",
            Self::Left => "left",
        }
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
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected)
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        matches!(self, Self::Banned)
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
            "pending" => Ok(Self::Pending),
            "rejected" => Ok(Self::Rejected),
            "banned" => Ok(Self::Banned),
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

// Database mapping: MemberStatus -> SMALLINT (1=active, 2=pending, 3=rejected, 4=banned, 5=left)
impl sqlx::Type<sqlx::Postgres> for MemberStatus {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for MemberStatus {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Active => 1,
            Self::Pending => 2,
            Self::Rejected => 3,
            Self::Banned => 4,
            Self::Left => 5,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for MemberStatus {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let val = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        match val {
            1 => Ok(Self::Active),
            2 => Ok(Self::Pending),
            3 => Ok(Self::Rejected),
            4 => Ok(Self::Banned),
            5 => Ok(Self::Left),
            _ => Err(format!("Invalid MemberStatus value: {val}").into()),
        }
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
            Self::Status => "rm.status",
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

    /// Member status (account state)
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

    /// Banned info
    pub banned_at: Option<DateTime<Utc>>,
    pub banned_by: Option<UserId>,
    pub banned_reason: Option<String>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for RoomMember {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        let banned_by: Option<String> = row.try_get("banned_by")?;
        Ok(Self {
            room_id: row.try_get("room_id")?,
            user_id: row.try_get("user_id")?,
            role: row.try_get("role")?,
            status: row.try_get("status")?,
            added_permissions: row.try_get::<i64, _>("added_permissions")? as u64,
            removed_permissions: row.try_get::<i64, _>("removed_permissions")? as u64,
            admin_added_permissions: row.try_get::<i64, _>("admin_added_permissions")? as u64,
            admin_removed_permissions: row.try_get::<i64, _>("admin_removed_permissions")? as u64,
            joined_at: row.try_get("joined_at")?,
            left_at: row.try_get("left_at")?,
            version: row.try_get("version")?,
            banned_at: row.try_get("banned_at")?,
            banned_by: banned_by.map(UserId::from_string),
            banned_reason: row.try_get("banned_reason")?,
        })
    }
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
        self.status.is_active() && self.left_at.is_none()
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
        if !self.status.is_active() {
            return false;
        }

        self.effective_permissions(role_default).has(permission)
    }

    pub fn leave(&mut self) {
        self.status = MemberStatus::Left;
        self.left_at = Some(Utc::now());
    }

    pub fn reject(&mut self) {
        self.status = MemberStatus::Rejected;
        self.left_at = Some(Utc::now());
        self.banned_at = None;
        self.banned_by = None;
        self.banned_reason = None;
    }

    /// Ban this member from the room
    ///
    /// Sets `left_at` to satisfy the DB constraint requiring `left_at IS NOT NULL`
    /// when `status = Banned`. A banned member is effectively no longer active in
    /// the room, so `left_at` records when the ban took effect.
    pub fn ban(&mut self, banned_by: UserId, reason: Option<String>) {
        let now = Utc::now();
        self.status = MemberStatus::Banned;
        self.left_at = Some(now);
        self.banned_at = Some(now);
        self.banned_by = Some(banned_by);
        self.banned_reason = reason;
    }

    /// Unban this member
    ///
    /// Clears `left_at` since the member is now active again.
    pub fn unban(&mut self) {
        self.status = MemberStatus::Active;
        self.left_at = None;
        self.banned_at = None;
        self.banned_by = None;
        self.banned_reason = None;
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
    pub is_online: bool,
    /// Whether the member is still active (has not left the room).
    /// Distinct from `is_online` which tracks WebSocket connection status.
    pub is_active: bool,
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
            left_at: None,
            version: 0,
            banned_at: self.banned_at,
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
        assert!(!MemberStatus::Active.is_banned());
        assert!(MemberStatus::Banned.is_banned());
        assert!(MemberStatus::Pending.is_pending());
        assert!(MemberStatus::Rejected.is_rejected());
    }

    #[test]
    fn test_member_is_active() {
        let member = test_member(RoomRole::Member);
        assert!(member.is_active());

        let mut left_member = test_member(RoomRole::Member);
        left_member.left_at = Some(Utc::now());
        assert!(!left_member.is_active());

        let mut banned_member = test_member(RoomRole::Member);
        banned_member.status = MemberStatus::Banned;
        assert!(!banned_member.is_active());

        let mut rejected_member = test_member(RoomRole::Member);
        rejected_member.reject();
        assert!(!rejected_member.is_active());
    }

    // ─── C1: ban() must set left_at ──────────────────────────────────

    #[test]
    fn test_ban_sets_left_at() {
        let mut member = test_member(RoomRole::Member);
        assert!(
            member.left_at.is_none(),
            "Precondition: left_at starts as None"
        );

        let banner = UserId("banner".to_string());
        member.ban(banner.clone(), Some("bad behavior".to_string()));

        assert_eq!(member.status, MemberStatus::Banned);
        assert!(
            member.left_at.is_some(),
            "ban() must set left_at to satisfy DB constraint (left_at IS NOT NULL when status=Banned)"
        );
        assert!(member.banned_at.is_some());
        assert_eq!(member.banned_by, Some(banner));
        assert_eq!(member.banned_reason, Some("bad behavior".to_string()));
    }

    #[test]
    fn test_unban_clears_left_at() {
        let mut member = test_member(RoomRole::Member);
        let banner = UserId("banner".to_string());
        member.ban(banner, None);
        assert!(member.left_at.is_some(), "Precondition: ban sets left_at");

        member.unban();

        assert_eq!(member.status, MemberStatus::Active);
        assert!(
            member.left_at.is_none(),
            "unban() must clear left_at since user is active again"
        );
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
    fn test_reject_sets_status_and_left_at_without_ban_metadata() {
        let mut member = test_member(RoomRole::Member);
        let banner = UserId("banner".to_string());
        member.ban(banner, Some("bad behavior".to_string()));

        member.reject();

        assert_eq!(member.status, MemberStatus::Rejected);
        assert!(member.left_at.is_some());
        assert!(member.banned_at.is_none());
        assert!(member.banned_by.is_none());
        assert!(member.banned_reason.is_none());
    }

    #[test]
    fn test_room_member_list_sort_by_as_sql() {
        assert_eq!(RoomMemberListSortBy::JoinedAt.as_sql(), "rm.joined_at");
        assert_eq!(RoomMemberListSortBy::Username.as_sql(), "u.username");
        assert_eq!(RoomMemberListSortBy::Role.as_sql(), "rm.role");
    }
}
