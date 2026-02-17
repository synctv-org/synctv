use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::id::{MediaId, PlaylistId, RoomId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoomPlaybackState {
    pub room_id: RoomId,
    pub playing_media_id: Option<MediaId>,
    pub playing_playlist_id: Option<PlaylistId>,
    pub relative_path: String,
    pub current_time: f64, // playback position in seconds
    pub speed: f64,        // 0.5, 1.0, 1.5, 2.0, etc.
    pub is_playing: bool,
    pub updated_at: DateTime<Utc>,
    pub version: i64, // For optimistic locking
}

impl RoomPlaybackState {
    #[must_use]
    pub fn new(room_id: RoomId) -> Self {
        Self {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            relative_path: String::new(),
            current_time: 0.0,
            speed: 1.0,
            is_playing: false,
            updated_at: Utc::now(),
            version: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_playback_state_new() {
        let room_id = RoomId::from_string("test_room_01".to_string());
        let state = RoomPlaybackState::new(room_id.clone());

        assert_eq!(state.room_id, room_id);
        assert!(state.playing_media_id.is_none());
        assert!(state.playing_playlist_id.is_none());
        assert!(state.relative_path.is_empty());
        assert!((state.current_time - 0.0).abs() < f64::EPSILON);
        assert!((state.speed - 1.0).abs() < f64::EPSILON);
        assert!(!state.is_playing);
        assert_eq!(state.version, 0);
    }

    #[test]
    fn test_playback_state_serialization_roundtrip() {
        let room_id = RoomId::from_string("test_room_02".to_string());
        let state = RoomPlaybackState::new(room_id);

        let json = serde_json::to_string(&state).expect("serialize");
        let deserialized: RoomPlaybackState =
            serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.room_id, state.room_id);
        assert!((deserialized.current_time - state.current_time).abs() < f64::EPSILON);
        assert!((deserialized.speed - state.speed).abs() < f64::EPSILON);
        assert_eq!(deserialized.is_playing, state.is_playing);
        assert_eq!(deserialized.version, state.version);
    }
}
