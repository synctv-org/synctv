//! Playlist model (directory/folder in tree structure)
//!
//! Design reference: external design doc 04-database-design.md §2.4.1

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::{
    id::{PlaylistId, RoomId, UserId},
    query::SortDirection,
};

sort_field_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PlaylistListSortBy {
        Name => { display: "name", sql: "name" },
        CreatedAt => { display: "created_at", sql: "created_at", aliases: ["createdat"] },
        UpdatedAt => { display: "updated_at", sql: "updated_at", aliases: ["updatedat"] },
        Position => { display: "position", sql: "position" },
    }
    default = Position;
    error = "Unknown playlist list sort field";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistListQuery {
    pub pagination: super::pagination::PageParams,
    pub search: Option<String>,
    pub source_provider: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Playlist {
    pub id: PlaylistId,
    pub room_id: RoomId,
    pub creator_id: Option<UserId>,
    pub name: String,
    pub parent_id: Option<PlaylistId>,
    pub position: f64,

    // Dynamic folder fields
    /// Provider type name for dynamic folders (e.g., "alist", "emby")
    /// NULL for static folders (manually added media)
    pub source_provider: Option<String>,
    pub source_config: Option<JsonValue>,
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
    pub parent_id: Option<PlaylistId>,

    // Dynamic folder fields
    pub source_provider: Option<String>,
    pub source_config: Option<JsonValue>,
    pub provider_instance_name: Option<String>,
}

/// Update playlist request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlaylistRequest {
    pub name: Option<String>,
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
    use chrono::Utc;

    fn make_playlist(
        name: &str,
        parent_id: Option<PlaylistId>,
        source_provider: Option<String>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(1),
            room_id: RoomId::expect_positive(1),
            creator_id: Some(UserId::expect_positive(1)),
            name: name.to_string(),
            parent_id,
            position: 0.0,
            source_provider,
            source_config: None,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn test_playlist_is_top_level() {
        let top_level = make_playlist("", None, None);
        assert!(top_level.is_top_level());

        let named = make_playlist("Music", None, None);
        assert!(named.is_top_level());

        let child = make_playlist("", Some(PlaylistId::expect_positive(2)), None);
        assert!(!child.is_top_level());
    }

    #[test]
    fn test_playlist_is_dynamic() {
        let dynamic = make_playlist(
            "Alist Folder",
            Some(PlaylistId::expect_positive(2)),
            Some("alist".to_string()),
        );
        assert!(dynamic.is_dynamic());
        assert!(!dynamic.is_static());

        let static_pl = make_playlist("Manual Folder", Some(PlaylistId::expect_positive(2)), None);
        assert!(!static_pl.is_dynamic());
        assert!(static_pl.is_static());
    }

    #[test]
    fn test_playlist_serde_roundtrip() {
        let playlist = make_playlist("Test", None, Some("emby".to_string()));
        let json = serde_json::to_value(&playlist).unwrap();
        let deserialized: Playlist = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.name, "Test");
        assert_eq!(deserialized.source_provider.as_deref(), Some("emby"));
    }

    #[test]
    fn test_create_playlist_request_deserialize() {
        let json = serde_json::json!({
            "room_id": 123,
            "name": "My Playlist",
            "parent_id": 456,
            "source_provider": "alist",
            "source_config": {"path": "/videos"},
            "provider_instance_name": "my_alist"
        });
        let req: CreatePlaylistRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.room_id.as_i64(), 123);
        assert_eq!(req.name, "My Playlist");
        assert_eq!(req.parent_id.as_ref().unwrap().as_i64(), 456);
        assert_eq!(req.source_provider.as_deref(), Some("alist"));
    }

    #[test]
    fn test_create_playlist_request_minimal() {
        let json = serde_json::json!({
            "room_id": 123,
            "name": "Simple"
        });
        let req: CreatePlaylistRequest = serde_json::from_value(json).unwrap();
        assert!(req.parent_id.is_none());
        assert!(req.source_provider.is_none());
        assert!(req.source_config.is_none());
    }

    #[test]
    fn test_update_playlist_request_partial() {
        let json = serde_json::json!({
            "name": "Renamed"
        });
        let req: UpdatePlaylistRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.name.as_deref(), Some("Renamed"));
    }

    #[test]
    fn test_playlist_with_count_serde() {
        let playlist = make_playlist("Counted", None, None);
        let pwc = PlaylistWithCount {
            playlist,
            media_count: 42,
            children_count: 3,
        };
        let json = serde_json::to_value(&pwc).unwrap();
        assert_eq!(json["media_count"], 42);
        assert_eq!(json["children_count"], 3);
        // Flattened playlist fields
        assert_eq!(json["name"], "Counted");
    }
}
