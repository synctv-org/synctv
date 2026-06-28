use sqlx::{PgConnection, PgPool};

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackProgress, RoomPlaybackState, UserId},
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

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn progress_for_source_with_executor(
        &self,
        room_id: &RoomId,
        media_id: Option<MediaId>,
        playlist_id: Option<PlaylistId>,
        target_hash: &str,
        conn: &mut PgConnection,
    ) -> Result<Option<RoomPlaybackProgress>> {
        let progress = sqlx::query_as!(
            RoomPlaybackProgress,
            r#"
            SELECT id,
                   room_id AS "room_id: RoomId",
                   media_id AS "media_id?: MediaId",
                   playlist_id AS "playlist_id?: PlaylistId",
                   target AS "target?: crate::models::ProviderTarget",
                   target_hash,
                   "position",
                   created_at,
                   updated_at,
                   version
            FROM room_playback_progress
            WHERE room_id = $1
              AND media_id IS NOT DISTINCT FROM $2
              AND playlist_id IS NOT DISTINCT FROM $3
              AND target_hash = $4
            "#,
            room_id as &RoomId,
            media_id as Option<MediaId>,
            playlist_id as Option<PlaylistId>,
            target_hash,
        )
        .fetch_optional(&mut *conn)
        .await?;

        Ok(progress)
    }

    async fn progress_by_id_with_executor(
        &self,
        progress_id: i64,
        room_id: &RoomId,
        conn: &mut PgConnection,
    ) -> Result<Option<RoomPlaybackProgress>> {
        let progress = sqlx::query_as!(
            RoomPlaybackProgress,
            r#"
            SELECT id,
                   room_id AS "room_id: RoomId",
                   media_id AS "media_id?: MediaId",
                   playlist_id AS "playlist_id?: PlaylistId",
                   target AS "target?: crate::models::ProviderTarget",
                   target_hash,
                   "position",
                   created_at,
                   updated_at,
                   version
            FROM room_playback_progress
            WHERE id = $1
              AND room_id = $2
            "#,
            progress_id,
            room_id as &RoomId,
        )
        .fetch_optional(&mut *conn)
        .await?;

        Ok(progress)
    }

    async fn create_progress_with_executor(
        &self,
        state: &RoomPlaybackState,
        conn: &mut PgConnection,
    ) -> Result<RoomPlaybackProgress> {
        let target_hash = state.target_hash()?;
        let progress = sqlx::query_as!(
            RoomPlaybackProgress,
            r#"
            INSERT INTO room_playback_progress (
                room_id,
                media_id,
                playlist_id,
                target,
                target_hash,
                "position",
                version
            )
            VALUES ($1, $2, $3, $4, $5, $6, 0)
            ON CONFLICT (
                room_id,
                COALESCE(media_id, 0),
                COALESCE(playlist_id, 0),
                target_hash
            )
            DO UPDATE SET target = EXCLUDED.target,
                          "position" = EXCLUDED."position",
                          version = room_playback_progress.version + 1
            RETURNING id,
                      room_id AS "room_id: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target AS "target?: crate::models::ProviderTarget",
                      target_hash,
                      "position",
                      created_at,
                      updated_at,
                      version
            "#,
            state.room_id as RoomId,
            state.playing_media_id as Option<MediaId>,
            state.playing_playlist_id as Option<PlaylistId>,
            state.target.as_ref() as Option<&crate::models::ProviderTarget>,
            target_hash,
            state.position,
        )
        .fetch_one(&mut *conn)
        .await?;

        Ok(progress)
    }

    async fn update_progress_position_with_executor(
        &self,
        progress_id: i64,
        room_id: &RoomId,
        position: f64,
        conn: &mut PgConnection,
    ) -> Result<RoomPlaybackProgress> {
        let progress = sqlx::query_as!(
            RoomPlaybackProgress,
            r#"
            UPDATE room_playback_progress
            SET "position" = $3,
                version = version + 1
            WHERE id = $1
              AND room_id = $2
            RETURNING id,
                      room_id AS "room_id: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target AS "target?: crate::models::ProviderTarget",
                      target_hash,
                      "position",
                      created_at,
                      updated_at,
                      version
            "#,
            progress_id,
            room_id as &RoomId,
            position,
        )
        .fetch_one(&mut *conn)
        .await?;

        Ok(progress)
    }

    async fn resolve_progress_for_state_with_executor(
        &self,
        state: &RoomPlaybackState,
        previous_progress_position: Option<f64>,
        conn: &mut PgConnection,
    ) -> Result<(Option<i64>, f64)> {
        let current_progress = match state.current_progress_id {
            Some(progress_id) => {
                self.progress_by_id_with_executor(progress_id, &state.room_id, conn)
                    .await?
            }
            None => None,
        };

        if state.playing_media_id.is_none() && state.playing_playlist_id.is_none() {
            if let Some(progress) = current_progress {
                let snapshot_position = previous_progress_position.unwrap_or(state.position);
                drop(
                    self.update_progress_position_with_executor(
                        progress.id,
                        &state.room_id,
                        snapshot_position,
                        conn,
                    )
                    .await?,
                );
            }
            return Ok((None, 0.0));
        }

        if let Some(progress) = current_progress {
            let target_hash = state.target_hash()?;
            if progress.media_id == state.playing_media_id
                && progress.playlist_id == state.playing_playlist_id
                && progress.target_hash.eq_ignore_ascii_case(&target_hash)
            {
                let progress = self
                    .update_progress_position_with_executor(
                        progress.id,
                        &state.room_id,
                        state.position,
                        conn,
                    )
                    .await?;
                return Ok((Some(progress.id), progress.position));
            }

            let snapshot_position = previous_progress_position.unwrap_or(state.position);
            drop(
                self.update_progress_position_with_executor(
                    progress.id,
                    &state.room_id,
                    snapshot_position,
                    conn,
                )
                .await?,
            );
        }

        let target_hash = state.target_hash()?;
        if let Some(progress) = self
            .progress_for_source_with_executor(
                &state.room_id,
                state.playing_media_id,
                state.playing_playlist_id,
                &target_hash,
                conn,
            )
            .await?
        {
            let progress = self
                .update_progress_position_with_executor(
                    progress.id,
                    &state.room_id,
                    state.position,
                    conn,
                )
                .await?;
            return Ok((Some(progress.id), progress.position));
        }

        let progress = self.create_progress_with_executor(state, conn).await?;
        Ok((Some(progress.id), progress.position))
    }

    async fn update_with_exact_version_on_conn(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
        previous_progress_position: Option<f64>,
        conn: &mut PgConnection,
    ) -> Result<RoomPlaybackState> {
        if new_version <= state.version {
            return Err(Error::InvalidInput(format!(
                "new playback version {new_version} must be greater than expected version {}",
                state.version
            )));
        }

        let (current_progress_id, _position) = self
            .resolve_progress_for_state_with_executor(state, previous_progress_position, conn)
            .await?;

        let result = sqlx::query_as!(
            RoomPlaybackState,
            r#"WITH updated AS (
                UPDATE room_playback_state
                SET playing_media_id = $2,
                    playing_playlist_id = $3,
                    target = $4,
                    current_progress_id = $5,
                    speed = $6,
                    is_playing = $7,
                    updated_at = NOW(),
                    version = $9
                WHERE room_id = $1 AND version = $8
                RETURNING room_id,
                          playing_media_id,
                          playing_playlist_id,
                          target,
                          current_progress_id,
                          speed,
                          is_playing,
                          updated_at,
                          version
            )
            SELECT updated.room_id AS "room_id: RoomId",
                   updated.playing_media_id AS "playing_media_id?: MediaId",
                   updated.playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                   updated.target AS "target?: crate::models::ProviderTarget",
                   updated.current_progress_id,
                   COALESCE(progress."position", 0.0) AS "position!",
                   updated.speed,
                   updated.is_playing,
                   updated.updated_at,
                   updated.version
            FROM updated
            LEFT JOIN room_playback_progress progress ON progress.id = updated.current_progress_id"#,
            state.room_id as RoomId,
            state.playing_media_id as Option<MediaId>,
            state.playing_playlist_id as Option<PlaylistId>,
            state.target.as_ref() as Option<&crate::models::ProviderTarget>,
            current_progress_id,
            state.speed,
            state.is_playing,
            state.version,
            new_version,
        )
        .fetch_optional(&mut *conn)
        .await?;

        match result {
            Some(state) => Ok(state),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Create or get playback state for room
    ///
    /// Uses `ON CONFLICT DO NOTHING` followed by a SELECT to avoid triggering
    /// the `updated_at` BEFORE UPDATE trigger on existing rows.
    pub async fn create_or_get(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let state = RoomPlaybackState::new(*room_id);

        // Attempt insert; if the row already exists, do nothing
        sqlx::query!(
            "INSERT INTO room_playback_state (room_id, speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (room_id) DO NOTHING",
            room_id as &RoomId,
            state.speed,
            state.is_playing,
            state.updated_at,
            state.version,
        )
        .execute(&self.pool)
        .await?;

        // Fetch the row (either just inserted or already existing)
        let result = sqlx::query_as!(
            RoomPlaybackState,
            r#"SELECT state.room_id AS "room_id: RoomId",
                      state.playing_media_id AS "playing_media_id?: MediaId",
                      state.playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                      state.target AS "target?: crate::models::ProviderTarget",
                      state.current_progress_id,
                      COALESCE(progress."position", 0.0) AS "position!",
                      state.speed,
                      state.is_playing,
                      state.updated_at,
                      state.version
             FROM room_playback_state state
             LEFT JOIN room_playback_progress progress ON progress.id = state.current_progress_id
             WHERE state.room_id = $1"#,
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(result)
    }

    /// Create or get playback state using a provided transaction
    ///
    /// Uses `ON CONFLICT DO NOTHING` followed by a SELECT to avoid triggering
    /// the `updated_at` BEFORE UPDATE trigger on existing rows.
    ///
    /// Accepts `&mut PgConnection` so the connection can be reborrowed for
    /// the follow-up SELECT query within the same transaction.
    pub async fn create_or_get_with_executor(
        &self,
        room_id: &RoomId,
        conn: &mut sqlx::PgConnection,
    ) -> Result<RoomPlaybackState> {
        let state = RoomPlaybackState::new(*room_id);

        sqlx::query!(
            "INSERT INTO room_playback_state (room_id, speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (room_id) DO NOTHING",
            room_id as &RoomId,
            state.speed,
            state.is_playing,
            state.updated_at,
            state.version,
        )
        .execute(&mut *conn)
        .await?;

        let result = sqlx::query_as!(
            RoomPlaybackState,
            r#"SELECT state.room_id AS "room_id: RoomId",
                      state.playing_media_id AS "playing_media_id?: MediaId",
                      state.playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                      state.target AS "target?: crate::models::ProviderTarget",
                      state.current_progress_id,
                      COALESCE(progress."position", 0.0) AS "position!",
                      state.speed,
                      state.is_playing,
                      state.updated_at,
                      state.version
             FROM room_playback_state state
             LEFT JOIN room_playback_progress progress ON progress.id = state.current_progress_id
             WHERE state.room_id = $1"#,
            room_id as &RoomId,
        )
        .fetch_one(&mut *conn)
        .await?;

        Ok(result)
    }

    /// Get playback state
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        let result = sqlx::query_as!(
            RoomPlaybackState,
            r#"SELECT state.room_id AS "room_id: RoomId",
                      state.playing_media_id AS "playing_media_id?: MediaId",
                      state.playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                      state.target AS "target?: crate::models::ProviderTarget",
                      state.current_progress_id,
                      COALESCE(progress."position", 0.0) AS "position!",
                      state.speed,
                      state.is_playing,
                      state.updated_at,
                      state.version
             FROM room_playback_state state
             LEFT JOIN room_playback_progress progress ON progress.id = state.current_progress_id
             WHERE state.room_id = $1"#,
            room_id as &RoomId,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// Run `update_with_exact_version_on_conn` inside a dedicated transaction,
    /// committing on success and rolling back on error.
    async fn update_in_tx(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
        previous_progress_position: Option<f64>,
    ) -> Result<RoomPlaybackState> {
        let mut tx = self.pool.begin().await?;
        let result = self
            .update_with_exact_version_on_conn(
                state,
                new_version,
                previous_progress_position,
                &mut tx,
            )
            .await;

        match result {
            Ok(state) => {
                tx.commit().await?;
                Ok(state)
            }
            Err(error) => {
                tx.rollback().await?;
                Err(error)
            }
        }
    }

    /// Update playback state with optimistic locking
    pub async fn update(&self, state: &RoomPlaybackState) -> Result<RoomPlaybackState> {
        self.update_in_tx(state, state.version + 1, None).await
    }

    /// Update playback state with optimistic locking and an externally allocated version.
    ///
    /// Strong-cache write paths reserve the next version from Redis first, then
    /// store that exact version in Postgres so Redis cannot lag behind the DB.
    pub async fn update_with_exact_version(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
    ) -> Result<RoomPlaybackState> {
        self.update_in_tx(state, new_version, None).await
    }

    pub async fn update_with_exact_version_and_previous_progress(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
        previous_progress_position: Option<f64>,
    ) -> Result<RoomPlaybackState> {
        self.update_in_tx(state, new_version, previous_progress_position)
            .await
    }

    pub async fn update_with_exact_version_executor(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
        conn: &mut PgConnection,
    ) -> Result<RoomPlaybackState> {
        self.update_with_exact_version_on_conn(state, new_version, None, conn)
            .await
    }

    pub async fn update_with_exact_version_executor_and_previous_progress(
        &self,
        state: &RoomPlaybackState,
        new_version: i64,
        previous_progress_position: Option<f64>,
        conn: &mut PgConnection,
    ) -> Result<RoomPlaybackState> {
        self.update_with_exact_version_on_conn(state, new_version, previous_progress_position, conn)
            .await
    }

    /// List playback states impacted by media/playlists owned by a creator.
    pub async fn find_playback_for_creator(
        &self,
        creator_id: &UserId,
    ) -> Result<Vec<RoomPlaybackState>> {
        self.find_playback_for_creator_with_executor(creator_id, &self.pool)
            .await
    }

    pub async fn find_playback_for_creator_with_executor<'e, E>(
        &self,
        creator_id: &UserId,
        executor: E,
    ) -> Result<Vec<RoomPlaybackState>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let states = sqlx::query_as!(
            RoomPlaybackState,
            r#"
            WITH impacted_rooms AS (
                SELECT DISTINCT rps.room_id
                FROM room_playback_state rps
                LEFT JOIN media m ON m.id = rps.playing_media_id
                LEFT JOIN playlists p ON p.id = rps.playing_playlist_id
                WHERE m.creator_id = $1 OR p.creator_id = $1
            )
            SELECT rps.room_id AS "room_id: RoomId",
                   rps.playing_media_id AS "playing_media_id?: MediaId",
                   rps.playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                   rps.target AS "target?: crate::models::ProviderTarget",
                   rps.current_progress_id,
                   COALESCE(progress."position", 0.0) AS "position!",
                   rps.speed,
                   rps.is_playing,
                   rps.updated_at,
                   rps.version
            FROM room_playback_state rps
            JOIN impacted_rooms impacted ON impacted.room_id = rps.room_id
            LEFT JOIN room_playback_progress progress ON progress.id = rps.current_progress_id
            FOR UPDATE OF rps
            "#,
            creator_id as &UserId,
        )
        .fetch_all(executor)
        .await?;

        Ok(states)
    }
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
