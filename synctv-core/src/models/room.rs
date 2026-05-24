use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt::Display;
use std::str::FromStr;

use super::id::{RoomId, UserId};
use super::permission::{
    RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits, RoomPermissionSet,
};
use super::query::SortDirection;
use crate::Error;

/// Derived room lifecycle used by API filters and display.
///
/// The canonical database state is `rooms.closed_at`: `NULL` means active,
/// non-`NULL` means closed. Creation review lives in `room_creation_requests`;
/// moderation bans live in `room_bans`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum RoomStatus {
    #[default]
    Active,
    Closed,
}

impl RoomStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    #[must_use]
    pub fn can_transition_to(&self, new_status: &Self) -> bool {
        matches!(
            (self, new_status),
            (a, b) if *a == *b
        ) || matches!(
            (self, new_status),
            (Self::Active, Self::Closed) | (Self::Closed, Self::Active)
        )
    }
}

impl FromStr for RoomStatus {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Self::Active),
            "closed" => Ok(Self::Closed),
            other => Err(format!("Unknown room status: {other}")),
        }
    }
}

impl std::fmt::Display for RoomStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<synctv_proto::common::RoomStatus> for RoomStatus {
    type Error = String;

    fn try_from(value: synctv_proto::common::RoomStatus) -> Result<Self, Self::Error> {
        match value {
            synctv_proto::common::RoomStatus::Active => Ok(Self::Active),
            synctv_proto::common::RoomStatus::Closed => Ok(Self::Closed),
            synctv_proto::common::RoomStatus::Unspecified => {
                Err(format!("Unknown room status: {}", value as i32))
            }
        }
    }
}

impl From<RoomStatus> for synctv_proto::common::RoomStatus {
    fn from(value: RoomStatus) -> Self {
        match value {
            RoomStatus::Active => Self::Active,
            RoomStatus::Closed => Self::Closed,
        }
    }
}

impl From<RoomStatus> for i32 {
    fn from(value: RoomStatus) -> Self {
        synctv_proto::common::RoomStatus::from(value) as Self
    }
}

impl TryFrom<i32> for RoomStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        let proto = synctv_proto::common::RoomStatus::try_from(value)
            .map_err(|_| format!("Unknown room status: {value}"))?;
        Self::try_from(proto)
    }
}

/// Playback mode for auto-play
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PlayMode {
    /// Sequential play (stop after last item)
    #[default]
    Sequential,
    /// Repeat single item
    RepeatOne,
    /// Repeat all items (loop back to start)
    RepeatAll,
    /// Random playback
    Shuffle,
}

/// Auto-play settings
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoPlaySettings {
    /// Whether auto-play is enabled
    pub enabled: bool,

    /// Playback mode
    pub mode: PlayMode,

    /// Delay before playing next item (seconds)
    pub delay: u32,
}

impl Default for AutoPlaySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: PlayMode::Sequential,
            delay: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    /// Room description (max 500 characters)
    #[serde(default)]
    pub description: String,
    /// Creator user ID. ON DELETE RESTRICT prevents deleting users who still own rooms.
    pub created_by: UserId,
    #[serde(skip)]
    pub status: RoomStatus,
    #[serde(skip)]
    pub is_banned: bool,
    /// Timestamp when the room was closed. `None` means the room is active.
    pub closed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Monotonically increasing integer for optimistic locking.
    /// Incremented on each UPDATE. Used by compare-and-increment to detect concurrent modifications.
    #[serde(default)]
    pub version: i32,
    /// Tracks the last significant activity in this room (chat messages,
    /// playback state changes, member joins/leaves). Used by the room TTL
    /// cleanup to avoid expiring active rooms.
    #[serde(default = "Utc::now")]
    pub last_activity_at: DateTime<Utc>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for Room {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;

        let closed_at: Option<DateTime<Utc>> = row.try_get("closed_at")?;
        let is_banned = row.try_get("is_banned")?;

        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            created_by: row.try_get("created_by")?,
            status: if closed_at.is_some() {
                RoomStatus::Closed
            } else {
                RoomStatus::Active
            },
            is_banned,
            closed_at,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            deleted_at: row.try_get("deleted_at")?,
            version: row.try_get("version")?,
            last_activity_at: row.try_get("last_activity_at")?,
        })
    }
}

impl Room {
    #[must_use]
    pub fn new(name: String, created_by: UserId) -> Self {
        let now = Utc::now();
        Self {
            id: RoomId::new(),
            name,
            description: String::new(),
            created_by,
            status: RoomStatus::Active,
            is_banned: false,
            closed_at: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
            last_activity_at: now,
        }
    }

    /// Create a new room with description
    #[must_use]
    pub fn new_with_description(name: String, description: String, created_by: UserId) -> Self {
        let now = Utc::now();
        Self {
            id: RoomId::new(),
            name,
            description,
            created_by,
            status: RoomStatus::Active,
            is_banned: false,
            closed_at: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
            last_activity_at: now,
        }
    }

    #[must_use]
    pub fn new_with_status(
        name: String,
        description: String,
        created_by: UserId,
        initial_status: RoomStatus,
    ) -> Self {
        let mut room = Self::new_with_description(name, description, created_by);
        room.status = initial_status;
        if initial_status == RoomStatus::Closed {
            room.closed_at = Some(room.created_at);
        }
        room
    }

    /// Check if room is open and not soft-deleted.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.closed_at.is_none() && self.deleted_at.is_none()
    }

    #[must_use]
    pub const fn status(&self) -> RoomStatus {
        if self.closed_at.is_some() {
            RoomStatus::Closed
        } else {
            RoomStatus::Active
        }
    }

    pub fn close(&mut self) {
        self.closed_at = Some(Utc::now());
        self.status = RoomStatus::Closed;
        self.updated_at = Utc::now();
    }

    pub fn reopen(&mut self) {
        self.closed_at = None;
        self.status = RoomStatus::Active;
        self.updated_at = Utc::now();
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.is_banned
    }

    pub fn ban(&mut self) {
        self.is_banned = true;
        self.updated_at = Utc::now();
    }

    pub fn unban(&mut self) {
        self.is_banned = false;
        self.updated_at = Utc::now();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRoomRequest {
    pub name: String,
    /// Room description (max 500 characters)
    #[serde(default)]
    pub description: String,
    pub password: Option<String>,
    pub settings: Option<JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRoomRequest {
    pub name: Option<String>,
    /// Room description (max 500 characters)
    pub description: Option<String>,
    pub closed: Option<bool>,
    pub settings: Option<JsonValue>,
}

/// Room with settings loaded from `room_settings` table
#[derive(Debug, Clone)]
pub struct RoomWithSettings {
    pub room: Room,
    pub settings: RoomSettingsJson,
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RoomListSortBy {
        Name => { display: "name", sql: "r.name" },
        UpdatedAt => { display: "updated_at", sql: "r.updated_at", aliases: ["updatedat"] },
        LastActivityAt => {
            display: "last_activity_at",
            sql: "r.last_activity_at",
            aliases: ["lastactivityat"]
        },
        CreatedAt => { display: "created_at", sql: "r.created_at", aliases: ["createdat"] },
    }
    default = CreatedAt;
    error = "Unknown room list sort field";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomListQuery {
    pub pagination: super::pagination::PageParams,
    pub status: Option<RoomStatus>,
    pub search: Option<String>,
    /// Filter by derived ban state from `room_bans`.
    #[serde(default)]
    pub is_banned: Option<bool>,
    /// Filter by creator
    pub creator_id: Option<super::UserId>,
    #[serde(default)]
    pub sort_by: RoomListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for RoomListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            status: Some(RoomStatus::Active),
            search: None,
            is_banned: Some(false),
            creator_id: None,
            sort_by: RoomListSortBy::CreatedAt,
            sort_direction: SortDirection::Desc,
        }
    }
}

/// Room settings for JSON serialization/deserialization (stored as JSON in database)
///
/// Note: For typed, registry-backed room settings, use `room_settings::RoomSettings` instead.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoomSettingsJson {
    pub require_password: bool,
    /// Auto-play settings
    #[serde(default)]
    pub auto_play: AutoPlaySettings,
    pub allow_guest_join: bool,
    /// Maximum number of members allowed in the room.
    /// `None` or `0` means no limit.  Uses `u32` to prevent negative values.
    pub max_members: Option<u32>,
    pub chat_enabled: bool,
    pub danmaku_enabled: bool,

    // Rooms can override global default permissions from SettingsRegistry
    // Each role has added/removed permissions that modify the global defaults
    // Formula: (global_default | added) & ~removed
    /// Additional permissions for Admin role (on top of global default)
    #[serde(default)]
    pub admin_added_permissions: Option<u64>,

    /// Removed permissions for Admin role (overrides global default)
    #[serde(default)]
    pub admin_removed_permissions: Option<u64>,

    /// Additional permissions for Member role (on top of global default)
    #[serde(default)]
    pub member_added_permissions: Option<u64>,

    /// Removed permissions for Member role (overrides global default)
    #[serde(default)]
    pub member_removed_permissions: Option<u64>,

    /// Additional permissions for Guests (on top of global default)
    #[serde(default)]
    pub guest_added_permissions: Option<u64>,

    /// Removed permissions for Guests (overrides global default)
    #[serde(default)]
    pub guest_removed_permissions: Option<u64>,

    /// Whether room requires approval for new members
    #[serde(default)]
    pub require_approval: bool,

    /// Whether members can auto-join (without invitation)
    #[serde(default = "default_true")]
    pub allow_auto_join: bool,
}

impl RoomSettingsJson {
    /// Calculate effective permissions for a role based on global defaults and room overrides
    ///
    /// Formula: (`global_default` | added) & ~removed
    ///
    /// Arguments:
    /// - `global_default`: Default permissions from global settings
    /// - `added_permissions`: Additional permissions from room settings (Optional)
    /// - `removed_permissions`: Removed permissions from room settings (Optional)
    #[must_use]
    pub const fn effective_permissions_for_role(
        global_default: RoomPermissionSet,
        added_permissions: Option<u64>,
        removed_permissions: Option<u64>,
    ) -> RoomPermissionSet {
        let mut result = global_default.0;

        // Add extra permissions
        if let Some(added) = added_permissions {
            result |= added;
        }

        // Remove permissions
        if let Some(removed) = removed_permissions {
            result &= !removed;
        }

        RoomPermissionSet(result)
    }

    /// Get effective permissions for Admin role
    ///
    /// Requires global default admin permissions from `SettingsRegistry`
    #[must_use]
    pub const fn admin_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0;
        if let Some(added) = self.admin_added_permissions {
            result |= RoomAdminPermissionBits::to_permissions(added);
        }
        if let Some(removed) = self.admin_removed_permissions {
            result &= !RoomAdminPermissionBits::to_permissions(removed);
        }
        RoomPermissionSet(result)
    }

    /// Get effective permissions for Member role
    ///
    /// Requires global default member permissions from `SettingsRegistry`
    #[must_use]
    pub const fn member_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0;
        if let Some(added) = self.member_added_permissions {
            result |= RoomMemberPermissionBits::to_permissions(added);
        }
        if let Some(removed) = self.member_removed_permissions {
            result &= !RoomMemberPermissionBits::to_permissions(removed);
        }
        RoomPermissionSet(result)
    }

    /// Get effective permissions for Guest
    ///
    /// Requires global default guest permissions from `SettingsRegistry`
    #[must_use]
    pub const fn guest_permissions(&self, global_default: RoomPermissionSet) -> RoomPermissionSet {
        let mut result = global_default.0 & RoomPermissionSet::guest_assignable().0;

        if let Some(added) = self.guest_added_permissions {
            result |= RoomGuestPermissionBits::to_permissions(added);
        }

        if let Some(removed) = self.guest_removed_permissions {
            result &= !RoomGuestPermissionBits::to_permissions(removed);
        }

        RoomPermissionSet(result)
    }
}

const fn default_true() -> bool {
    true
}

/// Room with member count (for efficient queries with JOIN)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomWithCount {
    #[serde(flatten)]
    pub room: Room,
    pub member_count: i32,
}

impl Display for PlayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sequential => write!(f, "sequential"),
            Self::RepeatOne => write!(f, "repeat_one"),
            Self::RepeatAll => write!(f, "repeat_all"),
            Self::Shuffle => write!(f, "shuffle"),
        }
    }
}

impl std::str::FromStr for PlayMode {
    type Err = crate::Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sequential" => Ok(Self::Sequential),
            "repeat_one" => Ok(Self::RepeatOne),
            "repeat_all" => Ok(Self::RepeatAll),
            "shuffle" => Ok(Self::Shuffle),
            other => Err(Error::InvalidInput(format!("Invalid PlayMode: {other}"))),
        }
    }
}

impl Display for AutoPlaySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use JSON representation for complex types
        let json = serde_json::to_string(self).map_err(|_| std::fmt::Error)?;
        write!(f, "{json}")
    }
}

impl std::str::FromStr for AutoPlaySettings {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| Error::InvalidInput(format!("Invalid AutoPlaySettings: {e}")))
    }
}

impl Display for RoomSettingsJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use JSON representation for the entire settings struct
        let json = serde_json::to_string(self).map_err(|_| std::fmt::Error)?;
        write!(f, "{json}")
    }
}

impl std::str::FromStr for RoomSettingsJson {
    type Err = crate::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
            .map_err(|e| Error::InvalidInput(format!("Invalid RoomSettingsJson: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_play_mode_parse_trimmed_case_insensitive_names() {
        assert_eq!(
            " sequential ".parse::<PlayMode>().unwrap(),
            PlayMode::Sequential
        );
        assert_eq!(
            " REPEAT_ONE ".parse::<PlayMode>().unwrap(),
            PlayMode::RepeatOne
        );
        assert_eq!(" shuffle ".parse::<PlayMode>().unwrap(), PlayMode::Shuffle);
    }

    #[test]
    fn room_status_proto_conversions_reject_unspecified_input() {
        assert_eq!(
            i32::from(RoomStatus::Active),
            synctv_proto::common::RoomStatus::Active as i32
        );
        assert_eq!(
            RoomStatus::try_from(synctv_proto::common::RoomStatus::Closed).unwrap(),
            RoomStatus::Closed
        );
        assert!(RoomStatus::try_from(synctv_proto::common::RoomStatus::Unspecified).is_err());
        assert!(RoomStatus::try_from(0).is_err());
    }

    #[test]
    fn test_status_transition_active_to_closed_is_valid() {
        assert!(RoomStatus::Active.can_transition_to(&RoomStatus::Closed));
    }

    #[test]
    fn test_status_transition_closed_to_active_is_valid() {
        assert!(RoomStatus::Closed.can_transition_to(&RoomStatus::Active));
    }

    #[test]
    fn test_status_transition_same_status_is_valid() {
        assert!(RoomStatus::Active.can_transition_to(&RoomStatus::Active));
        assert!(RoomStatus::Closed.can_transition_to(&RoomStatus::Closed));
    }

    #[test]
    fn test_status_transition_matrix_exhaustive() {
        let valid_transitions = [
            (RoomStatus::Active, RoomStatus::Closed),
            (RoomStatus::Closed, RoomStatus::Active),
        ];

        let all_statuses = [RoomStatus::Active, RoomStatus::Closed];

        for &from in &all_statuses {
            for &to in &all_statuses {
                let expected = from == to || valid_transitions.contains(&(from, to));
                assert_eq!(
                    from.can_transition_to(&to),
                    expected,
                    "Transition {:?} -> {:?} should be {}",
                    from,
                    to,
                    if expected { "valid" } else { "invalid" }
                );
            }
        }
    }
}
