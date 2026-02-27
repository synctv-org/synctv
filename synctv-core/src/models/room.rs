use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::fmt::Display;

use super::id::{RoomId, UserId};
use super::permission::PermissionBits;
use crate::Error;

/// Room lifecycle status (independent of ban state)
///
/// Status transitions:
/// - Active ↔ Closed: Room creator or admin can toggle
/// - Pending → Active: On first activity or explicit activation
///
/// Note: Banned state is tracked separately via `is_banned` field
/// to allow unbanning without losing the previous status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum RoomStatus {
    #[default]
    Active,
    Pending,
    Closed,
}

impl RoomStatus {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Pending => "pending",
            Self::Closed => "closed",
        }
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    /// Check if a status transition is valid.
    ///
    /// Valid transitions:
    /// - `Pending -> Active` (review approved)
    /// - `Pending -> Closed` (review rejected)
    /// - `Active -> Closed` (room closed)
    /// - `Closed -> Active` (room reopened)
    /// - Same status (no change) is always allowed
    ///
    /// Invalid transitions:
    /// - `Closed -> Pending` (cannot return to pending state)
    /// - `Active -> Pending` (cannot return to pending state)
    #[must_use]
    pub fn can_transition_to(&self, new_status: &Self) -> bool {
        match (self, new_status) {
            // Same status (no change) is always allowed
            (a, b) if *a == *b => true,

            // Pending can transition to Active (approved) or Closed (rejected)
            (Self::Pending, Self::Active) => true,
            (Self::Pending, Self::Closed) => true,

            // Active can transition to Closed (room closed)
            (Self::Active, Self::Closed) => true,

            // Closed can transition to Active (room reopened)
            (Self::Closed, Self::Active) => true,

            // All other transitions are invalid
            _ => false,
        }
    }
}

// Database mapping: RoomStatus -> SMALLINT
// Values: 1=active, 2=pending, 3=closed
impl sqlx::Type<sqlx::Postgres> for RoomStatus {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for RoomStatus {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        let val: i16 = match self {
            Self::Active => 1,
            Self::Pending => 2,
            Self::Closed => 3,
        };
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&val, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for RoomStatus {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let val = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        match val {
            1 => Ok(Self::Active),
            2 => Ok(Self::Pending),
            3 => Ok(Self::Closed),
            _ => Err(format!("Invalid RoomStatus value: {val}").into()),
        }
    }
}

// Conversion from proto RoomStatus to core RoomStatus
impl From<synctv_proto::common::RoomStatus> for RoomStatus {
    fn from(value: synctv_proto::common::RoomStatus) -> Self {
        match value {
            synctv_proto::common::RoomStatus::Active => Self::Active,
            synctv_proto::common::RoomStatus::Pending => Self::Pending,
            synctv_proto::common::RoomStatus::Closed => Self::Closed,
            synctv_proto::common::RoomStatus::Unspecified => Self::Active,
        }
    }
}

// Conversion from core RoomStatus to proto RoomStatus
impl From<RoomStatus> for synctv_proto::common::RoomStatus {
    fn from(value: RoomStatus) -> Self {
        match value {
            RoomStatus::Active => Self::Active,
            RoomStatus::Pending => Self::Pending,
            RoomStatus::Closed => Self::Closed,
        }
    }
}

// Conversion from core RoomStatus to i32 (via proto enum)
impl From<RoomStatus> for i32 {
    fn from(value: RoomStatus) -> Self {
        synctv_proto::common::RoomStatus::from(value) as Self
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Room {
    pub id: RoomId,
    pub name: String,
    /// Room description (max 500 characters)
    #[serde(default)]
    pub description: String,
    /// Creator user ID. Room is CASCADE-deleted when the creator is deleted.
    pub created_by: UserId,
    /// Room lifecycle status (Active/Pending/Closed)
    pub status: RoomStatus,
    /// Ban flag - independent of status, allows unbanning without losing previous status
    /// Only global admins can set/clear this flag
    #[serde(default)]
    pub is_banned: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Monotonically increasing integer for optimistic locking.
    /// Incremented on each UPDATE. Used by compare-and-increment to detect concurrent modifications.
    #[serde(default)]
    pub version: i32,
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
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
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
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        }
    }

    /// Create a new room with description and explicit status
    ///
    /// Used when `create_room_need_review=true` to create rooms in Pending status
    /// that require admin approval before becoming active.
    #[must_use]
    pub fn new_with_status(name: String, description: String, created_by: UserId, status: RoomStatus) -> Self {
        let now = Utc::now();
        Self {
            id: RoomId::new(),
            name,
            description,
            created_by,
            status,
            is_banned: false,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            version: 0,
        }
    }

    /// Check if room is usable (active status, not banned, not deleted)
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == RoomStatus::Active && !self.is_banned && self.deleted_at.is_none()
    }

    /// Check if room is banned
    #[must_use]
    pub const fn is_banned(&self) -> bool {
        self.is_banned
    }

    /// Ban the room (admin only)
    pub fn ban(&mut self) {
        self.is_banned = true;
        self.updated_at = Utc::now();
    }

    /// Unban the room, restoring previous status (admin only)
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
    pub status: Option<RoomStatus>,
    pub settings: Option<JsonValue>,
}

/// Room with settings loaded from `room_settings` table
#[derive(Debug, Clone)]
pub struct RoomWithSettings {
    pub room: Room,
    pub settings: RoomSettingsJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomListQuery {
    pub pagination: super::pagination::PageParams,
    pub status: Option<RoomStatus>,
    pub search: Option<String>,
    /// Filter by ban status (None = don't filter, Some(true) = banned only, Some(false) = not banned)
    #[serde(default)]
    pub is_banned: Option<bool>,
    /// Filter by creator
    pub creator_id: Option<String>,
}

impl Default for RoomListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            status: Some(RoomStatus::Active),
            search: None,
            is_banned: Some(false), // By default, exclude banned rooms
            creator_id: None,
        }
    }
}

/// Room settings for JSON serialization/deserialization (stored as JSON in database)
///
/// Note: For typed, registry-backed room settings, use `room_settings::RoomSettings` instead.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoomSettingsJson {
    pub require_password: bool,
    /// Auto-play settings (deprecated, use `auto_play`)
    #[serde(default)]
    pub auto_play_next: bool,
    /// Auto-play settings
    #[serde(default)]
    pub auto_play: AutoPlaySettings,
    /// Legacy: loop playlist (use `auto_play.mode` instead)
    #[serde(default)]
    pub loop_playlist: bool,
    /// Legacy: shuffle playlist (use `auto_play.mode` instead)
    #[serde(default)]
    pub shuffle_playlist: bool,
    pub allow_guest_join: bool,
    pub max_members: Option<i32>,
    pub chat_enabled: bool,
    pub danmaku_enabled: bool,

    // ===== Permission Override Configuration =====
    //
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
        global_default: PermissionBits,
        added_permissions: Option<u64>,
        removed_permissions: Option<u64>,
    ) -> PermissionBits {
        let mut result = global_default.0;

        // Add extra permissions
        if let Some(added) = added_permissions {
            result |= added;
        }

        // Remove permissions
        if let Some(removed) = removed_permissions {
            result &= !removed;
        }

        PermissionBits(result)
    }

    /// Get effective permissions for Admin role
    ///
    /// Requires global default admin permissions from `SettingsRegistry`
    #[must_use] 
    pub const fn admin_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        Self::effective_permissions_for_role(
            global_default,
            self.admin_added_permissions,
            self.admin_removed_permissions,
        )
    }

    /// Get effective permissions for Member role
    ///
    /// Requires global default member permissions from `SettingsRegistry`
    #[must_use] 
    pub const fn member_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        Self::effective_permissions_for_role(
            global_default,
            self.member_added_permissions,
            self.member_removed_permissions,
        )
    }

    /// Get effective permissions for Guest
    ///
    /// Requires global default guest permissions from `SettingsRegistry`
    #[must_use] 
    pub const fn guest_permissions(&self, global_default: PermissionBits) -> PermissionBits {
        Self::effective_permissions_for_role(
            global_default,
            self.guest_added_permissions,
            self.guest_removed_permissions,
        )
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

// ==================== Trait Implementations for Settings System ====================

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

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "sequential" => Ok(Self::Sequential),
            "repeat_one" => Ok(Self::RepeatOne),
            "repeat_all" => Ok(Self::RepeatAll),
            "shuffle" => Ok(Self::Shuffle),
            _ => Err(Error::InvalidInput(format!("Invalid PlayMode: {s}"))),
        }
    }
}

impl Display for AutoPlaySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use JSON representation for complex types
        let json = serde_json::to_string(self)
            .map_err(|_| std::fmt::Error)?;
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
        let json = serde_json::to_string(self)
            .map_err(|_| std::fmt::Error)?;
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
    fn test_status_transition_pending_to_active_is_valid() {
        assert!(RoomStatus::Pending.can_transition_to(&RoomStatus::Active));
    }

    #[test]
    fn test_status_transition_pending_to_closed_is_valid() {
        assert!(RoomStatus::Pending.can_transition_to(&RoomStatus::Closed));
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
        assert!(RoomStatus::Pending.can_transition_to(&RoomStatus::Pending));
        assert!(RoomStatus::Active.can_transition_to(&RoomStatus::Active));
        assert!(RoomStatus::Closed.can_transition_to(&RoomStatus::Closed));
    }

    #[test]
    fn test_status_transition_closed_to_pending_is_invalid() {
        assert!(!RoomStatus::Closed.can_transition_to(&RoomStatus::Pending));
    }

    #[test]
    fn test_status_transition_active_to_pending_is_invalid() {
        assert!(!RoomStatus::Active.can_transition_to(&RoomStatus::Pending));
    }

    #[test]
    fn test_status_transition_matrix_exhaustive() {
        // Define all valid transitions
        let valid_transitions = [
            (RoomStatus::Pending, RoomStatus::Active),
            (RoomStatus::Pending, RoomStatus::Closed),
            (RoomStatus::Active, RoomStatus::Closed),
            (RoomStatus::Closed, RoomStatus::Active),
        ];

        let all_statuses = [RoomStatus::Pending, RoomStatus::Active, RoomStatus::Closed];

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
