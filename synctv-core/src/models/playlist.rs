//! Playlist model for the room's playlist tree.
//!
//! Design reference: external design doc 04-database-design.md §2.4.1

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    id::{PlaylistId, RoomId, UserId},
    query::SortDirection,
    PlaylistSourceConfig, SourceProvider,
};

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PlaylistListSortBy {
        Name => { display: "name", sql: "name" },
        CreatedAt => { display: "created_at", sql: "created_at" },
        UpdatedAt => { display: "updated_at", sql: "updated_at" },
        Position => { display: "position", sql: "position" },
    }
    default = Position;
    error = "Unknown playlist list sort field";
}

/// Controls who can browse a playlist from a room client.
///
/// `Default` is resolved from the playlist kind: static playlists are browsable
/// to room actors with `BROWSE_LIBRARY`, while dynamic playlists are restricted
/// to their creator. The value is intentionally stored as an integer without a
/// database constraint; unknown values are rejected when loaded by the domain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistBrowseAccessMode {
    #[default]
    Default = 0,
    RoomMembers = 1,
    CreatorOnly = 2,
}

i16_enum!(
    PlaylistBrowseAccessMode,
    "Unknown playlist browse access mode",
    { Default = 0, RoomMembers = 1, CreatorOnly = 2 }
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub source_provider: Option<SourceProvider>,
    pub provider_instance_name: Option<String>,
    pub dynamic_only: Option<bool>,
    pub availability: Option<bool>,
    #[serde(default)]
    pub sort_by: PlaylistListSortBy,
    #[serde(default)]
    pub sort_direction: SortDirection,
}

impl Default for PlaylistListQuery {
    fn default() -> Self {
        Self {
            pagination: super::pagination::PageParams::default(),
            search: None,
            source_provider: None,
            provider_instance_name: None,
            dynamic_only: None,
            availability: None,
            sort_by: PlaylistListSortBy::Position,
            sort_direction: SortDirection::Asc,
        }
    }
}

/// A room playlist, optionally nested under another playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: PlaylistId,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    #[serde(default)]
    pub browse_access_mode: PlaylistBrowseAccessMode,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub cover_file_reference_id: Option<i64>,
    pub parent_id: Option<PlaylistId>,
    pub position: f64,

    // Dynamic playlist fields
    pub source_provider: Option<SourceProvider>,
    pub source_config: Option<PlaylistSourceConfig>,
    pub provider_instance_name: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    /// Optimistic locking version (incremented on each update)
    pub version: i32,
}

impl Playlist {
    /// Check if this playlist is directly under the room root.
    #[must_use]
    pub const fn is_top_level(&self) -> bool {
        self.parent_id.is_none()
    }

    /// Check if this is a dynamic playlist
    #[must_use]
    pub const fn is_dynamic(&self) -> bool {
        self.source_provider.is_some()
    }

    #[must_use]
    pub const fn effective_browse_access_mode(&self) -> PlaylistBrowseAccessMode {
        match self.browse_access_mode {
            PlaylistBrowseAccessMode::Default if self.is_dynamic() => {
                PlaylistBrowseAccessMode::CreatorOnly
            }
            PlaylistBrowseAccessMode::Default => PlaylistBrowseAccessMode::RoomMembers,
            mode => mode,
        }
    }

    #[must_use]
    pub fn is_accessible_to(&self, viewer_id: Option<UserId>) -> bool {
        match self.effective_browse_access_mode() {
            PlaylistBrowseAccessMode::RoomMembers => viewer_id.is_some(),
            PlaylistBrowseAccessMode::CreatorOnly => {
                viewer_id.is_some() && self.creator_id == viewer_id
            }
            PlaylistBrowseAccessMode::Default => false,
        }
    }
}

/// Create playlist request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePlaylistRequest {
    pub room_id: RoomId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub parent_id: Option<PlaylistId>,
    #[serde(default)]
    pub browse_access_mode: PlaylistBrowseAccessMode,

    // Dynamic playlist fields
    pub source_provider: Option<SourceProvider>,
    pub source_config: Option<PlaylistSourceConfig>,
    pub provider_instance_name: Option<String>,
}

/// Playlist with media count (for efficient queries)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistWithCount {
    #[serde(flatten)]
    pub playlist: Playlist,
    pub media_count: i64,
    pub children_count: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_ok<T>(result: serde_json::Result<T>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => std::panic::panic_any(format!("{context}: {error}")),
        }
    }

    fn make_playlist(
        name: &str,
        parent_id: Option<PlaylistId>,
        source_provider: Option<SourceProvider>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(1),
            room_id: RoomId::expect_positive(1),
            creator_id: Some(UserId::expect_positive(1)),
            browse_access_mode: PlaylistBrowseAccessMode::Default,
            name: name.to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id,
            position: 0.0,
            source_provider,
            source_config: None,
            provider_instance_name: None,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
            version: 0,
        }
    }

    #[test]
    fn create_playlist_request_defaults_description() {
        let json = serde_json::json!({
            "room_id": 123,
            "name": "Simple"
        });
        let req: CreatePlaylistRequest = json_ok(
            serde_json::from_value(json),
            "playlist request should deserialize",
        );
        assert_eq!(req.description, "");
        assert!(req.parent_id.is_none());
        assert!(req.source_provider.is_none());
        assert!(req.source_config.is_none());
    }

    #[test]
    fn playlist_with_count_flattens_playlist_fields() {
        let playlist = make_playlist("Counted", None, None);
        let pwc = PlaylistWithCount {
            playlist,
            media_count: 42,
            children_count: 3,
        };
        let json = json_ok(
            serde_json::to_value(&pwc),
            "playlist count should serialize",
        );
        assert_eq!(json["media_count"], 42);
        assert_eq!(json["children_count"], 3);
        assert_eq!(json["name"], "Counted");
    }

    #[test]
    fn browse_access_mode_defaults_follow_playlist_kind() {
        let static_playlist = make_playlist("Static", None, None);
        assert_eq!(
            static_playlist.effective_browse_access_mode(),
            PlaylistBrowseAccessMode::RoomMembers
        );
        assert!(!static_playlist.is_accessible_to(None));

        let dynamic_playlist = make_playlist("Dynamic", None, Some(SourceProvider::Alist));
        assert_eq!(
            dynamic_playlist.effective_browse_access_mode(),
            PlaylistBrowseAccessMode::CreatorOnly
        );
        assert!(dynamic_playlist.is_accessible_to(dynamic_playlist.creator_id));
        assert!(!dynamic_playlist.is_accessible_to(Some(UserId::expect_positive(99))));
        assert!(!dynamic_playlist.is_accessible_to(None));
    }

    #[test]
    fn room_members_mode_allows_authenticated_dynamic_viewers() {
        let mut playlist = make_playlist("Dynamic", None, Some(SourceProvider::Alist));
        playlist.browse_access_mode = PlaylistBrowseAccessMode::RoomMembers;

        assert!(playlist.is_accessible_to(Some(UserId::expect_positive(99))));
        assert!(!playlist.is_accessible_to(None));
    }

    #[test]
    fn creator_only_does_not_grant_access_to_unowned_playlists() {
        let mut playlist = make_playlist("Static", None, None);
        playlist.browse_access_mode = PlaylistBrowseAccessMode::CreatorOnly;

        assert!(!playlist.is_accessible_to(None));
    }

    #[test]
    fn creator_only_mode_applies_to_static_playlists() {
        let mut playlist = make_playlist("Static", None, None);
        playlist.browse_access_mode = PlaylistBrowseAccessMode::CreatorOnly;

        assert!(playlist.is_accessible_to(playlist.creator_id));
        assert!(!playlist.is_accessible_to(Some(UserId::expect_positive(99))));
        assert!(!playlist.is_accessible_to(None));
    }
}
