use sqlx::PgPool;

use crate::{
    models::{RoomId, RoomPlaybackState},
    Error, Result,
};

/// Room playback state repository
#[derive(Clone)]
pub struct RoomPlaybackStateRepository {
    pool: PgPool,
}

impl RoomPlaybackStateRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create or get playback state for room
    ///
    /// Uses `ON CONFLICT DO UPDATE` to always return via RETURNING, avoiding
    /// the TOCTOU race of a separate check-then-insert pattern.
    pub async fn create_or_get(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let state = RoomPlaybackState::new(room_id.clone());

        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "INSERT INTO room_playback_state (room_id, current_time, speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (room_id) DO UPDATE SET room_id = EXCLUDED.room_id
             RETURNING room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version"
        )
        .bind(room_id.as_str())
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.updated_at)
        .bind(state.version)
        .fetch_one(&self.pool)
        .await?;

        Ok(result)
    }

    /// Create or get playback state using a provided executor (pool or transaction)
    pub async fn create_or_get_with_executor<'e, E>(&self, room_id: &RoomId, executor: E) -> Result<RoomPlaybackState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let state = RoomPlaybackState::new(room_id.clone());

        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "INSERT INTO room_playback_state (room_id, current_time, speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (room_id) DO UPDATE SET room_id = EXCLUDED.room_id
             RETURNING room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version"
        )
        .bind(room_id.as_str())
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.updated_at)
        .bind(state.version)
        .fetch_one(executor)
        .await?;

        Ok(result)
    }

    /// Get playback state
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "SELECT room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version
             FROM room_playback_state
             WHERE room_id = $1",
        )
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// Update playback state with optimistic locking
    pub async fn update(&self, state: &RoomPlaybackState) -> Result<RoomPlaybackState> {
        let media_id_str = state.playing_media_id.as_ref().map(super::super::models::id::MediaId::as_str);
        let playlist_id_str = state.playing_playlist_id.as_ref().map(super::super::models::id::PlaylistId::as_str);

        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "UPDATE room_playback_state
             SET playing_media_id = $2, playing_playlist_id = $3, relative_path = $4,
                 current_time = $5, speed = $6, is_playing = $7,
                 updated_at = NOW(), version = version + 1
             WHERE room_id = $1 AND version = $8
             RETURNING room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version",
        )
        .bind(state.room_id.as_str())
        .bind(media_id_str)
        .bind(playlist_id_str)
        .bind(&state.relative_path)
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.version)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(s) => Ok(s),
            None => Err(Error::OptimisticLockConflict),
        }
    }
}

#[cfg(test)]
mod tests {

}
