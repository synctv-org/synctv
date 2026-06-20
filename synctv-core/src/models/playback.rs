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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlaybackDurationStatus {
    Unknown,
    Pending,
    Available,
    Unavailable,
    Failed,
}

sqlx_i16_enum!(PlaybackDurationStatus, "invalid playback duration status", {
    Unknown = 0,
    Pending = 1,
    Available = 2,
    Unavailable = 3,
    Failed = 4,
});

impl PlaybackDurationStatus {
    #[must_use]
    pub fn claimable_initial_statuses() -> [i16; 3] {
        [
            Self::Unknown.into(),
            Self::Failed.into(),
            Self::Unavailable.into(),
        ]
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlaybackDurationSource {
    Provider,
    Probe,
}

sqlx_i16_enum!(PlaybackDurationSource, "invalid playback duration source", {
    Provider = 1,
    Probe = 2,
});

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, sqlx::FromRow)]
pub struct PlaybackSourceMetadata {
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target_hash: String,
    pub duration_seconds: Option<f64>,
    pub duration_status: PlaybackDurationStatus,
    pub duration_source: Option<PlaybackDurationSource>,
    pub duration_error: Option<String>,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedPlaybackDurationProbe {
    pub metadata: PlaybackSourceMetadata,
    pub state: RoomPlaybackState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlaybackSourceIdentity {
    pub room_id: RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target_hash: String,
}

impl PlaybackSourceIdentity {
    #[must_use]
    pub fn from_state(state: &RoomPlaybackState) -> Option<Self> {
        if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
            return None;
        }

        Some(Self {
            room_id: state.room_id,
            media_id: state.playing_media_id,
            playlist_id: state.playing_playlist_id,
            target_hash: state.target_hash(),
        })
    }

    #[must_use]
    pub fn static_media(room_id: RoomId, media_id: MediaId) -> Self {
        Self {
            room_id,
            media_id: Some(media_id),
            playlist_id: None,
            target_hash: hash_playback_target(&[]),
        }
    }

    #[must_use]
    pub fn dynamic_playlist(room_id: RoomId, playlist_id: PlaylistId, target: &[u8]) -> Self {
        Self {
            room_id,
            media_id: None,
            playlist_id: Some(playlist_id),
            target_hash: hash_playback_target(target),
        }
    }
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
        hash_playback_target(&self.target)
    }
}

#[must_use]
pub fn hash_playback_target(target: &[u8]) -> String {
    hex::encode(Sha256::digest(target))
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
