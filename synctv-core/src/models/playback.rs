use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::id::{MediaId, PlaylistId, RoomId};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct RoomPlaybackState {
    pub room_id: RoomId,
    pub playing_media_id: Option<MediaId>,
    pub playing_playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
    pub current_progress_id: Option<i64>,
    pub position: f64, // playback position in seconds
    pub speed: f64,    // 0.5, 1.0, 1.5, 2.0, etc.
    pub is_playing: bool,
    pub updated_at: DateTime<Utc>,
    pub version: i64, // For optimistic locking
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct RoomPlaybackProgress {
    pub id: i64,
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Vec<u8>,
    pub target_hash: String,
    pub position: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

impl RoomPlaybackState {
    #[must_use]
    pub fn new(room_id: RoomId) -> Self {
        Self {
            room_id,
            playing_media_id: None,
            playing_playlist_id: None,
            target: Vec::new(),
            current_progress_id: None,
            position: 0.0,
            speed: 1.0,
            is_playing: false,
            updated_at: Utc::now(),
            version: 0,
        }
    }

    /// Computes the server-side playback position from the persisted anchor.
    #[must_use]
    pub fn computed_position(&self) -> f64 {
        if self.is_playing {
            let delta = Utc::now() - self.updated_at;
            let elapsed = delta
                .to_std()
                .map_or(0.0, |duration| duration.as_secs_f64());
            self.position + elapsed * self.speed
        } else {
            self.position
        }
    }

    #[must_use]
    pub fn target_hash(&self) -> String {
        hex::encode(Sha256::digest(&self.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computed_position_advances_while_playing() {
        let mut state = RoomPlaybackState::new(RoomId::expect_positive(70_001));
        state.position = 30.0;
        state.speed = 2.0;
        state.is_playing = true;
        state.updated_at = Utc::now() - chrono::Duration::seconds(5);

        let position = state.computed_position();

        assert!(
            position >= 39.0,
            "computed position should include elapsed playback time, got {position}"
        );
    }

    #[test]
    fn computed_position_uses_anchor_while_paused() {
        let mut state = RoomPlaybackState::new(RoomId::expect_positive(70_002));
        state.position = 120.5;
        state.speed = 2.0;
        state.is_playing = false;
        state.updated_at = Utc::now() - chrono::Duration::seconds(30);

        assert!((state.computed_position() - 120.5).abs() < f64::EPSILON);
    }

    #[test]
    fn computed_position_clamps_negative_elapsed_time() {
        let mut state = RoomPlaybackState::new(RoomId::expect_positive(70_003));
        state.position = 45.0;
        state.speed = 1.5;
        state.is_playing = true;
        state.updated_at = Utc::now() + chrono::Duration::seconds(30);

        assert!((state.computed_position() - 45.0).abs() < f64::EPSILON);
    }
}
