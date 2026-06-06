use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use super::id::{RoomId, UserId};
use super::permission::{
    Role as RoomRole, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits,
    RoomPermission, RoomPermissionSet,
};
use super::query::SortDirection;
use super::room::RoomStatus;

/// Current room membership state.
///
/// `room_members` is active-only: if a row exists, the user is currently a
/// member. Leave, kick, and user purge physically delete rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    #[default]
    Active,
}

impl MemberStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        "active"
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        true
    }
}

impl FromStr for MemberStatus {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            other => Err(format!("Unknown member status: {other}")),
        }
    }
}

impl std::fmt::Display for MemberStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<MemberStatus> for synctv_proto::common::MemberStatus {
    fn from(_: MemberStatus) -> Self {
        Self::Active
    }
}

impl From<MemberStatus> for i32 {
    fn from(value: MemberStatus) -> Self {
        synctv_proto::common::MemberStatus::from(value) as Self
    }
}

impl TryFrom<synctv_proto::common::MemberStatus> for MemberStatus {
    type Error = String;

    fn try_from(value: synctv_proto::common::MemberStatus) -> Result<Self, Self::Error> {
        match value {
            synctv_proto::common::MemberStatus::Active => Ok(Self::Active),
            synctv_proto::common::MemberStatus::Unspecified => {
                Err(format!("Unknown member status: {}", value as i32))
            }
        }
    }
}

impl TryFrom<i32> for MemberStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        let proto = synctv_proto::common::MemberStatus::try_from(value)
            .map_err(|_| format!("Unknown member status: {value}"))?;
        Self::try_from(proto)
    }
}

/// Repository-level options for admitting a user into a room membership row.
#[derive(Debug, Clone, Default)]
pub struct AddMemberOptions {
    /// Check if room is active
    pub check_room_active: bool,
    /// Check for duplicate membership
    pub check_duplicate: bool,
    /// Check max members limit
    pub check_max_members: bool,
    /// Maximum number of members allowed (0 = no limit)
    pub max_members: u64,
    /// Invalidate permission cache after adding
    pub invalidate_cache: bool,
}

impl AddMemberOptions {
    /// Create default options (all checks enabled, no max limit)
    #[must_use]
    pub const fn new() -> Self {
        Self {
            check_room_active: true,
            check_duplicate: true,
            check_max_members: false,
            max_members: 0,
            invalidate_cache: true,
        }
    }

    /// Set max members limit (enables the check)
    #[must_use]
    pub const fn with_max_members(mut self, max: u64) -> Self {
        self.max_members = max;
        self.check_max_members = true;
        self
    }

    /// Skip max members check
    #[must_use]
    pub const fn skip_max_members_check(mut self) -> Self {
        self.check_max_members = false;
        self
    }

    /// Skip room active check
    #[must_use]
    pub const fn skip_active_check(mut self) -> Self {
        self.check_room_active = false;
        self
    }

    /// Skip duplicate membership check
    #[must_use]
    pub const fn skip_duplicate_check(mut self) -> Self {
        self.check_duplicate = false;
        self
    }

    /// Skip cache invalidation
    #[must_use]
    pub const fn skip_cache_invalidation(mut self) -> Self {
        self.invalidate_cache = false;
        self
    }
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RoomMemberListSortBy {
        Username => { display: "username", sql: "u.username" },
        Role => { display: "role", sql: "rm.role" },
        JoinedAt => { display: "joined_at", sql: "rm.joined_at" },
    }
    default = JoinedAt;
    error = "Unknown room member list sort field";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMemberListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub role: Option<RoomRole>,
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
            is_online: None,
            sort_by: RoomMemberListSortBy::JoinedAt,
            sort_direction: SortDirection::Asc,
        }
    }
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum MyRoomListSortBy {
        Name => { display: "name", sql: "r.name" },
        CreatedAt => { display: "created_at", sql: "r.created_at" },
        UpdatedAt => { display: "updated_at", sql: "r.updated_at" },
        LastActivityAt => {
            display: "last_activity_at",
            sql: "r.last_activity_at"
        },
        JoinedAt => { display: "joined_at", sql: "rm.joined_at" },
    }
    default = JoinedAt;
    error = "Unknown related room list field";
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
    pub role: RoomRole,
    #[serde(skip)]
    pub status: MemberStatus,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
    pub joined_at: DateTime<Utc>,
    pub version: i64,
}

impl RoomMember {
    #[must_use]
    pub fn new(room_id: RoomId, user_id: UserId, role: RoomRole) -> Self {
        Self {
            room_id,
            user_id,
            role,
            status: MemberStatus::Active,
            added_permissions: 0,
            removed_permissions: 0,
            admin_added_permissions: 0,
            admin_removed_permissions: 0,
            joined_at: Utc::now(),
            version: 0,
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        true
    }

    #[must_use]
    pub const fn status(&self) -> MemberStatus {
        MemberStatus::Active
    }

    #[must_use]
    pub const fn effective_permissions(
        &self,
        role_default: RoomPermissionSet,
    ) -> RoomPermissionSet {
        match self.role {
            RoomRole::Creator => RoomPermissionSet::all(),
            RoomRole::Admin => {
                let mut result = role_default.0;
                result |= RoomAdminPermissionBits::to_permissions(self.admin_added_permissions);
                result &= !RoomAdminPermissionBits::to_permissions(self.admin_removed_permissions);
                RoomPermissionSet(result)
            }
            RoomRole::Member => {
                let mut result = role_default.0;
                result |= RoomMemberPermissionBits::to_permissions(self.added_permissions);
                result &= !RoomMemberPermissionBits::to_permissions(self.removed_permissions);
                RoomPermissionSet(result)
            }
            RoomRole::Guest => {
                let mut result = role_default.0 & RoomPermissionSet::guest_assignable().0;
                result |= RoomGuestPermissionBits::to_permissions(self.added_permissions);
                result &= !RoomGuestPermissionBits::to_permissions(self.removed_permissions);
                RoomPermissionSet(result)
            }
        }
    }

    #[must_use]
    pub const fn has_permission(
        &self,
        permission: RoomPermission,
        role_default: RoomPermissionSet,
    ) -> bool {
        self.effective_permissions(role_default).has(permission)
    }

    pub const fn add_permissions(&mut self, permissions: u64) {
        self.added_permissions |= permissions;
    }

    pub const fn remove_permissions(&mut self, permissions: u64) {
        self.removed_permissions |= permissions;
    }

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
    pub is_active: bool,
}

impl RoomMemberWithUser {
    #[must_use]
    pub fn effective_permissions(&self, role_default: RoomPermissionSet) -> RoomPermissionSet {
        let member = RoomMember {
            room_id: self.room_id,
            user_id: self.user_id,
            role: self.role,
            status: self.status,
            added_permissions: self.added_permissions,
            removed_permissions: self.removed_permissions,
            admin_added_permissions: self.admin_added_permissions,
            admin_removed_permissions: self.admin_removed_permissions,
            joined_at: self.joined_at,
            version: 0,
        };

        member.effective_permissions(role_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_member(role: RoomRole) -> RoomMember {
        RoomMember::new(RoomId::expect_positive(1), UserId::expect_positive(1), role)
    }

    #[test]
    fn test_creator_always_has_all_permissions() {
        let member = test_member(RoomRole::Creator);
        let result = member.effective_permissions(RoomPermissionSet::empty());
        assert_eq!(result.0, RoomPermissionSet::all().0);
    }

    #[test]
    fn test_member_with_added_permissions() {
        let mut member = test_member(RoomRole::Member);
        member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;
        let default = RoomPermissionSet::default_member();
        let result = member.effective_permissions(default);
        assert!(result.has(crate::models::RoomPermission::USE_WEBRTC));
        assert!(result.has(crate::models::RoomPermission::CHAT));
    }

    #[test]
    fn test_member_with_removed_permissions() {
        let mut member = test_member(RoomRole::Member);
        member.removed_permissions = RoomMemberPermissionBits::CHAT;
        let default = RoomPermissionSet::default_member();
        let result = member.effective_permissions(default);
        assert!(!result.has(crate::models::RoomPermission::CHAT));
        assert!(result.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    }

    #[test]
    fn test_admin_uses_admin_overrides() {
        let mut member = test_member(RoomRole::Admin);
        member.admin_added_permissions = RoomAdminPermissionBits::PLAY_CONTROL;
        member.admin_removed_permissions = RoomAdminPermissionBits::KICK_MEMBER;
        let default = RoomPermissionSet::default_member();
        let result = member.effective_permissions(default);
        assert!(result.has(crate::models::RoomPermission::PLAY_CONTROL));
        assert!(!result.has(crate::models::RoomPermission::KICK_MEMBER));
    }

    #[test]
    fn test_guest_rejects_added_chat() {
        let mut member = test_member(RoomRole::Guest);
        member.added_permissions = RoomMemberPermissionBits::CHAT;
        let default = RoomPermissionSet::default_guest();
        let result = member.effective_permissions(default);
        assert!(!result.has(crate::models::RoomPermission::CHAT));
        assert!(!result.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    }

    #[test]
    fn test_guest_accepts_guest_assignable_override() {
        let mut member = test_member(RoomRole::Guest);
        member.added_permissions = RoomGuestPermissionBits::USE_WEBRTC;
        let default = RoomPermissionSet::default_guest();
        let result = member.effective_permissions(default);
        assert!(result.has(crate::models::RoomPermission::USE_WEBRTC));
    }

    #[test]
    fn member_status_proto_conversions_reject_unspecified_input() {
        assert_eq!(
            i32::from(MemberStatus::Active),
            synctv_proto::common::MemberStatus::Active as i32
        );
        assert!(MemberStatus::try_from(synctv_proto::common::MemberStatus::Unspecified).is_err());
        assert!(MemberStatus::try_from(0).is_err());
    }

    #[test]
    fn test_member_is_active() {
        let member = test_member(RoomRole::Member);
        assert!(member.is_active());
    }

    #[test]
    fn test_room_member_list_sort_by_as_sql() {
        assert_eq!(RoomMemberListSortBy::JoinedAt.as_sql(), "rm.joined_at");
        assert_eq!(RoomMemberListSortBy::Username.as_sql(), "u.username");
        assert_eq!(RoomMemberListSortBy::Role.as_sql(), "rm.role");
    }
}
