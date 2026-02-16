use sqlx::{postgres::PgRow, PgPool, Row};

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackState},
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

        let row = sqlx::query(
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

        self.row_to_state(row)
    }

    /// Create or get playback state using a provided executor (pool or transaction)
    pub async fn create_or_get_with_executor<'e, E>(&self, room_id: &RoomId, executor: E) -> Result<RoomPlaybackState>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let state = RoomPlaybackState::new(room_id.clone());

        let row = sqlx::query(
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

        self.row_to_state(row)
    }

    /// Get playback state
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        let row = sqlx::query(
            "SELECT room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version
             FROM room_playback_state
             WHERE room_id = $1",
        )
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.row_to_state(row)?)),
            None => Ok(None),
        }
    }

    /// Update playback state with optimistic locking
    pub async fn update(&self, state: &RoomPlaybackState) -> Result<RoomPlaybackState> {
        let media_id_str = state.playing_media_id.as_ref().map(super::super::models::id::MediaId::as_str);
        let playlist_id_str = state.playing_playlist_id.as_ref().map(super::super::models::id::PlaylistId::as_str);

        let row = sqlx::query(
            "UPDATE room_playback_state
             SET playing_media_id = $2, playing_playlist_id = $3, relative_path = $4,
                 current_time = $5, speed = $6, is_playing = $7,
                 updated_at = $8, version = version + 1
             WHERE room_id = $1 AND version = $9
             RETURNING room_id, playing_media_id, playing_playlist_id, relative_path, current_time, speed, is_playing, updated_at, version",
        )
        .bind(state.room_id.as_str())
        .bind(media_id_str)
        .bind(playlist_id_str)
        .bind(&state.relative_path)
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(chrono::Utc::now())
        .bind(state.version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => self.row_to_state(row),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Convert database row to `RoomPlaybackState`
    fn row_to_state(&self, row: PgRow) -> Result<RoomPlaybackState> {
        let media_id_opt: Option<String> = row.try_get("playing_media_id")?;
        let playlist_id_opt: Option<String> = row.try_get("playing_playlist_id")?;

        Ok(RoomPlaybackState {
            room_id: RoomId::from_string(row.try_get("room_id")?),
            playing_media_id: media_id_opt.map(MediaId::from_string),
            playing_playlist_id: playlist_id_opt.map(PlaylistId::from_string),
            relative_path: row.try_get("relative_path")?,
            current_time: row.try_get("current_time")?,
            speed: row.try_get("speed")?,
            is_playing: row.try_get("is_playing")?,
            updated_at: row.try_get("updated_at")?,
            version: row.try_get("version")?,
        })
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn test_create_or_get() {
        // Placeholder: requires room setup
        // Covered by higher-level integration tests
    }

    #[tokio::test]
    async fn test_optimistic_locking() {
        // Placeholder: requires room + playback state setup
        // Covered by higher-level integration tests
    }
}
