//! Playlist model (directory/folder in tree structure)
//!
//! Design reference: external design doc 04-database-design.md §2.4.1

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    PlaylistSourceConfig, SourceProvider,
    id::{PlaylistId, RoomId, UserId},
    query::SortDirection,
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

/// Playlist (directory/folder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: PlaylistId,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub cover_file_reference_id: Option<i64>,
    pub parent_id: Option<PlaylistId>,
    pub position: f64,

    // Dynamic folder fields
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

    /// Check if this is a dynamic folder
    #[must_use]
    pub const fn is_dynamic(&self) -> bool {
        self.source_provider.is_some()
    }

    /// Check if this is a static folder
    #[must_use]
    pub const fn is_static(&self) -> bool {
        self.source_provider.is_none()
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

    // Dynamic folder fields
    pub source_provider: Option<SourceProvider>,
    pub source_config: Option<PlaylistSourceConfig>,
    pub provider_instance_name: Option<String>,
}

/// Update playlist request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlaylistRequest {
    pub name: Option<String>,
    pub description: Option<String>,
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
}
