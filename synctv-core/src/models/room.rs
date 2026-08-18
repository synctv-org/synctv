use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::str::FromStr;

use super::id::{RoomCategoryId, RoomId, RoomLabelId, UserId};
use super::query::SortDirection;
use super::RoomSettings;
use crate::Error;

fn default_last_activity_at() -> DateTime<Utc> {
    crate::SystemClock.now()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomCategory {
    pub id: RoomCategoryId,
    pub key: String,
    pub name: String,
    pub description: String,
    pub sort_order: i32,
    pub is_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomLabel {
    pub id: RoomLabelId,
    pub key: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub category_id: Option<RoomCategoryId>,
    pub sort_order: i32,
    pub is_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertRoomCategory {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "default_enabled")]
    pub is_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertRoomLabel {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: String,
    pub category_id: Option<RoomCategoryId>,
    #[serde(default)]
    pub sort_order: i32,
    #[serde(default = "default_enabled")]
    pub is_enabled: bool,
}

const fn default_enabled() -> bool {
    true
}

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

impl From<RoomStatus> for i32 {
    fn from(value: RoomStatus) -> Self {
        match value {
            RoomStatus::Active => 1,
            RoomStatus::Closed => 2,
        }
    }
}

impl TryFrom<i32> for RoomStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Active),
            2 => Ok(Self::Closed),
            _ => Err(format!("Unknown room status: {value}")),
        }
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
    pub cover_file_reference_id: Option<i64>,
    #[serde(default)]
    pub category: Option<RoomCategory>,
    #[serde(default)]
    pub labels: Vec<RoomLabel>,
    /// Creator user ID. ON DELETE RESTRICT prevents deleting users who still own rooms.
    pub created_by: UserId,
    #[serde(skip)]
    pub status: RoomStatus,
    #[serde(skip)]
    pub is_banned: bool,
    /// Whether the room is listed in discovery and available to anonymous guests.
    #[serde(default = "default_enabled")]
    pub is_public: bool,
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
    #[serde(default = "default_last_activity_at")]
    pub last_activity_at: DateTime<Utc>,
}

impl Room {
    #[must_use]
    pub fn new(name: String, created_by: UserId) -> Self {
        let now = crate::SystemClock.now();
        Self {
            id: RoomId::new(),
            name,
            description: String::new(),
            cover_file_reference_id: None,
            category: None,
            labels: Vec::new(),
            created_by,
            status: RoomStatus::Active,
            is_banned: false,
            is_public: true,
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
        let now = crate::SystemClock.now();
        Self {
            id: RoomId::new(),
            name,
            description,
            cover_file_reference_id: None,
            category: None,
            labels: Vec::new(),
            created_by,
            status: RoomStatus::Active,
            is_banned: false,
            is_public: true,
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
        self.closed_at = Some(crate::SystemClock.now());
        self.status = RoomStatus::Closed;
        self.updated_at = crate::SystemClock.now();
    }

    pub fn reopen(&mut self) {
        self.closed_at = None;
        self.status = RoomStatus::Active;
        self.updated_at = crate::SystemClock.now();
    }

    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.is_banned
    }

    pub fn ban(&mut self) {
        self.is_banned = true;
        self.updated_at = crate::SystemClock.now();
    }

    pub fn unban(&mut self) {
        self.is_banned = false;
        self.updated_at = crate::SystemClock.now();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRoomRequest {
    pub name: String,
    /// Room description (max 500 characters)
    #[serde(default)]
    pub description: String,
    pub password: Option<String>,
    pub settings: Option<RoomSettings>,
    pub category_id: Option<RoomCategoryId>,
    #[serde(default)]
    pub label_ids: Vec<RoomLabelId>,
    #[serde(default = "default_enabled")]
    pub is_public: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRoomRequest {
    pub name: Option<String>,
    /// Room description (max 500 characters)
    pub description: Option<String>,
    pub closed: Option<bool>,
    pub settings: Option<RoomSettings>,
    pub category_id: Option<RoomCategoryId>,
    pub label_ids: Option<Vec<RoomLabelId>>,
    pub is_public: Option<bool>,
}

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum RoomListSortBy {
        Name => { display: "name", sql: "r.name" },
        UpdatedAt => { display: "updated_at", sql: "r.updated_at" },
        LastActivityAt => {
            display: "last_activity_at",
            sql: "r.last_activity_at"
        },
        CreatedAt => { display: "created_at", sql: "r.created_at" },
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
    /// Filter by public discovery visibility.
    #[serde(default)]
    pub is_public: Option<bool>,
    /// Filter by creator
    pub creator_id: Option<super::UserId>,
    pub category_id: Option<RoomCategoryId>,
    #[serde(default)]
    pub label_ids: Vec<RoomLabelId>,
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
            is_public: None,
            creator_id: None,
            category_id: None,
            label_ids: Vec::new(),
            sort_by: RoomListSortBy::CreatedAt,
            sort_direction: SortDirection::Desc,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    #[test]
    fn test_play_mode_parse_trimmed_case_insensitive_names() {
        assert_eq!(
            ok(
                " sequential ".parse::<PlayMode>(),
                "sequential play mode should parse"
            ),
            PlayMode::Sequential
        );
        assert_eq!(
            ok(
                " REPEAT_ONE ".parse::<PlayMode>(),
                "repeat-one play mode should parse"
            ),
            PlayMode::RepeatOne
        );
        assert_eq!(
            ok(
                " shuffle ".parse::<PlayMode>(),
                "shuffle play mode should parse"
            ),
            PlayMode::Shuffle
        );
    }

    #[test]
    fn room_status_i32_conversions_reject_unknown_input() {
        assert_eq!(i32::from(RoomStatus::Active), 1);
        assert_eq!(
            ok(RoomStatus::try_from(2), "closed room status should convert"),
            RoomStatus::Closed
        );
        assert!(RoomStatus::try_from(0).is_err());
        assert!(RoomStatus::try_from(3).is_err());
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
