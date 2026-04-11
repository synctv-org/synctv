use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::id::{MediaId, PlaylistId, RoomId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct RoomPlaybackState {
    pub room_id: RoomId,
    pub playing_media_id: Option<MediaId>,
    pub playing_playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
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
            target: Vec::new(),
            current_time: 0.0,
            speed: 1.0,
            is_playing: false,
            updated_at: Utc::now(),
            version: 0,
        }
    }

    /// Compute the current playback time accounting for elapsed wall-clock time.
    ///
    /// When playback is active (`is_playing == true`), the stored `current_time`
    /// becomes stale immediately after the last DB write.  This method extrapolates
    /// the position using `speed` and the time elapsed since `updated_at`.
    ///
    /// # NTP / clock adjustment caveat
    ///
    /// This calculation uses `Utc::now()` which is subject to NTP clock adjustments.
    /// If the system clock jumps backward, the elapsed time could be negative (clamped
    /// to 0.0 below). If it jumps forward, the computed position will overshoot.
    /// Clients should use their own local monotonic clock for smooth playback
    /// interpolation and treat this server-side value as a periodic sync reference.
    #[must_use]
    pub fn computed_current_time(&self) -> f64 {
        if self.is_playing {
            let delta = Utc::now() - self.updated_at;
            let elapsed = delta
                .to_std()
                .map_or(0.0, |duration| duration.as_secs_f64());
            self.current_time + elapsed * self.speed
        } else {
            self.current_time
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
        assert!(state.target.is_empty());
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
        let deserialized: RoomPlaybackState = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.room_id, state.room_id);
        assert!((deserialized.current_time - state.current_time).abs() < f64::EPSILON);
        assert!((deserialized.speed - state.speed).abs() < f64::EPSILON);
        assert_eq!(deserialized.is_playing, state.is_playing);
        assert_eq!(deserialized.version, state.version);
    }
}
