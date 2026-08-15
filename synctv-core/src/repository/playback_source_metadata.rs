use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{
        ClaimedPlaybackDurationProbe, MediaId, PlaybackDurationSource, PlaybackDurationStatus,
        PlaybackKind, PlaybackSourceIdentity, PlaybackSourceMetadata, PlaylistId, ProviderTarget,
        RoomId, RoomPlaybackState,
    },
    Result,
};

#[derive(Clone)]
pub struct PlaybackSourceMetadataRepository {
    pool: PgPool,
}

#[derive(Debug, sqlx::FromRow)]
struct PlaybackSourceWithStateRow {
    metadata_room_id: RoomId,
    metadata_media_id: Option<MediaId>,
    metadata_playlist_id: Option<PlaylistId>,
    metadata_target_hash: String,
    metadata_media_name: Option<String>,
    metadata_playlist_name: Option<String>,
    metadata_playback_kind: PlaybackKind,
    duration_seconds: Option<f64>,
    metadata_duration_status: PlaybackDurationStatus,
    metadata_duration_source: Option<PlaybackDurationSource>,
    duration_error: Option<String>,
    next_retry_at: Option<DateTime<Utc>>,
    metadata_created_at: DateTime<Utc>,
    metadata_updated_at: DateTime<Utc>,
    metadata_version: i64,
    state_room_id: RoomId,
    state_playing_media_id: Option<MediaId>,
    state_playing_playlist_id: Option<PlaylistId>,
    state_target: Option<ProviderTarget>,
    state_current_progress_id: Option<i64>,
    state_history_cursor_id: Option<i64>,
    state_position: f64,
    state_speed: f64,
    state_is_playing: bool,
    state_playback_generation: i64,
    state_updated_at: DateTime<Utc>,
    state_version: i64,
}

impl PlaybackSourceWithStateRow {
    fn into_parts(self) -> (PlaybackSourceMetadata, RoomPlaybackState) {
        let metadata = PlaybackSourceMetadata {
            room_id: self.metadata_room_id,
            media_id: self.metadata_media_id,
            playlist_id: self.metadata_playlist_id,
            target_hash: self.metadata_target_hash,
            media_name: self.metadata_media_name,
            playlist_name: self.metadata_playlist_name,
            playback_kind: self.metadata_playback_kind,
            duration_seconds: self.duration_seconds,
            duration_status: self.metadata_duration_status,
            duration_source: self.metadata_duration_source,
            duration_error: self.duration_error,
            next_retry_at: self.next_retry_at,
            created_at: self.metadata_created_at,
            updated_at: self.metadata_updated_at,
            version: self.metadata_version,
        };
        let state = RoomPlaybackState {
            room_id: self.state_room_id,
            playing_media_id: self.state_playing_media_id,
            playing_playlist_id: self.state_playing_playlist_id,
            target: self.state_target,
            current_progress_id: self.state_current_progress_id,
            history_cursor_id: self.state_history_cursor_id,
            position: self.state_position,
            speed: self.state_speed,
            is_playing: self.state_is_playing,
            playback_generation: self.state_playback_generation,
            updated_at: self.state_updated_at,
            version: self.state_version,
        };
        (metadata, state)
    }
}

impl PlaybackSourceMetadataRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<Option<PlaybackSourceMetadata>> {
        let metadata = sqlx::query_as!(
            PlaybackSourceMetadata,
            r#"
            SELECT room_id AS "room_id!: RoomId",
                   media_id AS "media_id?: MediaId",
                   playlist_id AS "playlist_id?: PlaylistId",
                   target_hash,
                   media_name,
                   playlist_name,
                   playback_kind AS "playback_kind!: PlaybackKind",
                   duration_seconds,
                   duration_status AS "duration_status!: PlaybackDurationStatus",
                   duration_source AS "duration_source?: PlaybackDurationSource",
                   duration_error,
                   next_retry_at,
                   created_at,
                   updated_at,
                   version
            FROM playback_source_metadata
            WHERE room_id = $1
              AND media_id IS NOT DISTINCT FROM $2
              AND playlist_id IS NOT DISTINCT FROM $3
              AND target_hash = $4
            "#,
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn upsert_provider_source_metadata(
        &self,
        identity: &PlaybackSourceIdentity,
        playback_kind: PlaybackKind,
        duration_seconds: Option<f64>,
        media_name: Option<&str>,
        playlist_name: Option<&str>,
    ) -> Result<PlaybackSourceMetadata> {
        let (duration_status, duration_source) = if duration_seconds.is_some() {
            (
                PlaybackDurationStatus::Available,
                Some(PlaybackDurationSource::Provider),
            )
        } else {
            (PlaybackDurationStatus::Unavailable, None)
        };

        let metadata = sqlx::query_as!(
            PlaybackSourceMetadata,
            r#"
            INSERT INTO playback_source_metadata (
                room_id,
                media_id,
                playlist_id,
                target_hash,
                media_name,
                playlist_name,
                playback_kind,
                duration_seconds,
                duration_status,
                duration_source,
                duration_error,
                next_retry_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, NULL)
            ON CONFLICT (
                room_id,
                COALESCE(media_id, 0),
                COALESCE(playlist_id, 0),
                target_hash
            )
            DO UPDATE SET media_name = COALESCE(EXCLUDED.media_name, playback_source_metadata.media_name),
                          playlist_name = COALESCE(EXCLUDED.playlist_name, playback_source_metadata.playlist_name),
                          playback_kind = EXCLUDED.playback_kind,
                          duration_seconds = EXCLUDED.duration_seconds,
                          duration_status = EXCLUDED.duration_status,
                          duration_source = EXCLUDED.duration_source,
                          duration_error = CASE
                              WHEN playback_source_metadata.playback_kind IS DISTINCT FROM EXCLUDED.playback_kind
                                OR playback_source_metadata.duration_seconds IS DISTINCT FROM EXCLUDED.duration_seconds
                                OR playback_source_metadata.duration_status IS DISTINCT FROM EXCLUDED.duration_status
                                OR playback_source_metadata.duration_source IS DISTINCT FROM EXCLUDED.duration_source
                              THEN NULL
                              ELSE playback_source_metadata.duration_error
                          END,
                          next_retry_at = CASE
                              WHEN playback_source_metadata.playback_kind IS DISTINCT FROM EXCLUDED.playback_kind
                                OR playback_source_metadata.duration_seconds IS DISTINCT FROM EXCLUDED.duration_seconds
                                OR playback_source_metadata.duration_status IS DISTINCT FROM EXCLUDED.duration_status
                                OR playback_source_metadata.duration_source IS DISTINCT FROM EXCLUDED.duration_source
                              THEN NULL
                              ELSE playback_source_metadata.next_retry_at
                          END,
                          version = CASE
                              WHEN playback_source_metadata.playback_kind IS DISTINCT FROM EXCLUDED.playback_kind
                                OR playback_source_metadata.duration_seconds IS DISTINCT FROM EXCLUDED.duration_seconds
                                OR playback_source_metadata.duration_status IS DISTINCT FROM EXCLUDED.duration_status
                                OR playback_source_metadata.duration_source IS DISTINCT FROM EXCLUDED.duration_source
                              THEN playback_source_metadata.version + 1
                              ELSE playback_source_metadata.version
                          END
            WHERE playback_source_metadata.playback_kind IS DISTINCT FROM EXCLUDED.playback_kind
               OR playback_source_metadata.duration_seconds IS DISTINCT FROM EXCLUDED.duration_seconds
               OR playback_source_metadata.duration_status IS DISTINCT FROM EXCLUDED.duration_status
               OR playback_source_metadata.duration_source IS DISTINCT FROM EXCLUDED.duration_source
               OR playback_source_metadata.media_name IS DISTINCT FROM EXCLUDED.media_name
               OR playback_source_metadata.playlist_name IS DISTINCT FROM EXCLUDED.playlist_name
               OR playback_source_metadata.updated_at <= NOW() - INTERVAL '60 seconds'
            RETURNING room_id AS "room_id!: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target_hash,
                      media_name,
                      playlist_name,
                      playback_kind AS "playback_kind!: PlaybackKind",
                      duration_seconds,
                      duration_status AS "duration_status!: PlaybackDurationStatus",
                      duration_source AS "duration_source?: PlaybackDurationSource",
                      duration_error,
                      next_retry_at,
                      created_at,
                      updated_at,
                      version
            "#,
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            media_name,
            playlist_name,
            i16::from(playback_kind),
            duration_seconds,
            i16::from(duration_status),
            duration_source.map(i16::from),
        )
        .fetch_optional(&self.pool)
        .await?;

        match metadata {
            Some(metadata) => Ok(metadata),
            None => self.get(identity).await?.ok_or_else(|| {
                crate::Error::Internal(
                    "playback source metadata upsert returned no row".to_string(),
                )
            }),
        }
    }

    pub async fn mark_probeable_unknown_if_absent(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<PlaybackSourceMetadata> {
        let metadata = sqlx::query_as!(
            PlaybackSourceMetadata,
            r#"
            INSERT INTO playback_source_metadata (
                room_id,
                media_id,
                playlist_id,
                target_hash,
                playback_kind,
                duration_status
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (
                room_id,
                COALESCE(media_id, 0),
                COALESCE(playlist_id, 0),
                target_hash
            )
            DO UPDATE SET duration_status = playback_source_metadata.duration_status,
                          version = playback_source_metadata.version
            RETURNING room_id AS "room_id!: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target_hash,
                      media_name,
                      playlist_name,
                      playback_kind AS "playback_kind!: PlaybackKind",
                      duration_seconds,
                      duration_status AS "duration_status!: PlaybackDurationStatus",
                      duration_source AS "duration_source?: PlaybackDurationSource",
                      duration_error,
                      next_retry_at,
                      created_at,
                      updated_at,
                      version
            "#,
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            i16::from(PlaybackKind::Regular),
            i16::from(PlaybackDurationStatus::Unknown),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn update_names_if_present(
        &self,
        identity: &PlaybackSourceIdentity,
        media_name: Option<&str>,
        playlist_name: Option<&str>,
    ) -> Result<()> {
        sqlx::query!(
            r#"UPDATE playback_source_metadata
               SET media_name = COALESCE($5, media_name),
                   playlist_name = COALESCE($6, playlist_name),
                   updated_at = CURRENT_TIMESTAMP,
                   version = version + 1
               WHERE room_id = $1
                 AND media_id IS NOT DISTINCT FROM $2
                 AND playlist_id IS NOT DISTINCT FROM $3
                 AND target_hash = $4
                 AND (media_name IS DISTINCT FROM COALESCE($5, media_name)
                   OR playlist_name IS DISTINCT FROM COALESCE($6, playlist_name))"#,
            identity.room_id.as_i64(),
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            media_name,
            playlist_name,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_active_finite_sources_for_rooms(
        &self,
        room_ids: &[RoomId],
        limit: i64,
    ) -> Result<Vec<(PlaybackSourceMetadata, RoomPlaybackState)>> {
        // Auto-advance starts from rooms active in the current process. Keep
        // the room filter in SQL so the worker stays node-local. The progress
        // target_hash join binds metadata to the current target, including
        // dynamic playlist entries whose playlist id stays the same while the
        // target changes.
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        let room_ids = room_ids.iter().map(RoomId::as_i64).collect::<Vec<_>>();
        let rows = sqlx::query_as!(
            PlaybackSourceWithStateRow,
            r#"
            SELECT metadata.room_id AS "metadata_room_id!: RoomId",
                   metadata.media_id AS "metadata_media_id?: MediaId",
                   metadata.playlist_id AS "metadata_playlist_id?: PlaylistId",
                   metadata.target_hash AS "metadata_target_hash!",
                   metadata.media_name AS metadata_media_name,
                   metadata.playlist_name AS metadata_playlist_name,
                   metadata.playback_kind AS "metadata_playback_kind!: PlaybackKind",
                   metadata.duration_seconds,
                   metadata.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   metadata.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   metadata.duration_error,
                   metadata.next_retry_at,
                   metadata.created_at AS "metadata_created_at!",
                   metadata.updated_at AS "metadata_updated_at!",
                   metadata.version AS "metadata_version!",
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS "state_target?: ProviderTarget",
                   state.current_progress_id AS state_current_progress_id,
                   state.history_cursor_id AS state_history_cursor_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS "state_speed!",
                   state.is_playing AS "state_is_playing!",
                   state.playback_generation AS "state_playback_generation!",
                   state.updated_at AS "state_updated_at!",
                   state.version AS "state_version!"
            FROM room_playback_state state
            JOIN room_playback_progress progress ON progress.id = state.current_progress_id
            JOIN playback_source_metadata metadata
              ON metadata.room_id = state.room_id
             AND metadata.media_id IS NOT DISTINCT FROM state.playing_media_id
             AND metadata.playlist_id IS NOT DISTINCT FROM state.playing_playlist_id
             AND metadata.target_hash = progress.target_hash
            WHERE state.is_playing = TRUE
              AND state.room_id = ANY($2)
              AND metadata.duration_status = $1
              AND metadata.duration_seconds IS NOT NULL
            ORDER BY state.updated_at ASC
            LIMIT $3
            "#,
            i16::from(PlaybackDurationStatus::Available),
            &room_ids,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;

        let sources = rows
            .into_iter()
            .map(PlaybackSourceWithStateRow::into_parts)
            .filter(|(metadata, _)| metadata.playback_kind.allows_auto_advance())
            .collect();

        Ok(sources)
    }

    pub async fn claim_duration_probe_batch_for_rooms(
        &self,
        room_ids: &[RoomId],
        limit: i64,
    ) -> Result<Vec<ClaimedPlaybackDurationProbe>> {
        // Duration probing is scheduled per node from local active rooms, then
        // claimed in the primary database. FOR UPDATE SKIP LOCKED is the
        // cross-node concurrency control when several nodes host the same room.
        // The current room playback progress target_hash is part of the join so
        // dynamic playlist probes apply only to the item that is currently
        // playing. Keep this query on checked SQLx macros and update `.sqlx`
        // with SQL changes; offline query-shape validation is part of the
        // worker safety net.
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        let room_ids = room_ids.iter().map(RoomId::as_i64).collect::<Vec<_>>();
        let claimable_initial_statuses = PlaybackDurationStatus::claimable_initial_statuses();
        let rows = sqlx::query_as!(
            PlaybackSourceWithStateRow,
            r#"
            WITH claimed AS (
                SELECT metadata.room_id,
                       metadata.media_id,
                       metadata.playlist_id,
                       metadata.target_hash
                FROM playback_source_metadata metadata
                JOIN room_playback_state state
                  ON state.room_id = metadata.room_id
                 AND state.playing_media_id IS NOT DISTINCT FROM metadata.media_id
                 AND state.playing_playlist_id IS NOT DISTINCT FROM metadata.playlist_id
                JOIN room_playback_progress progress
                  ON progress.id = state.current_progress_id
                 AND progress.target_hash = metadata.target_hash
                WHERE state.room_id = ANY($5)
                  AND (
                      metadata.duration_status = ANY($1::smallint[])
                      OR (
                          metadata.duration_status = $2
                          AND metadata.next_retry_at <= NOW()
                      )
                )
                  AND state.is_playing = TRUE
                  AND metadata.playback_kind = $6
                  AND metadata.duration_seconds IS NULL
                  AND (metadata.next_retry_at IS NULL OR metadata.next_retry_at <= NOW())
                ORDER BY metadata.updated_at ASC
                LIMIT $3
                FOR UPDATE OF metadata SKIP LOCKED
            ),
            updated AS (
                UPDATE playback_source_metadata metadata
                   SET duration_status = $4,
                       duration_error = NULL,
                       next_retry_at = NOW() + INTERVAL '5 minutes',
                       version = metadata.version + 1
                  FROM claimed
                 WHERE metadata.room_id = claimed.room_id
                   AND metadata.media_id IS NOT DISTINCT FROM claimed.media_id
                   AND metadata.playlist_id IS NOT DISTINCT FROM claimed.playlist_id
                   AND metadata.target_hash = claimed.target_hash
                RETURNING metadata.room_id,
                          metadata.media_id,
                          metadata.playlist_id,
                          metadata.target_hash,
                          metadata.media_name,
                          metadata.playlist_name,
                          metadata.playback_kind,
                          metadata.duration_seconds,
                          metadata.duration_status,
                          metadata.duration_source,
                          metadata.duration_error,
                          metadata.next_retry_at,
                          metadata.created_at,
                          metadata.updated_at,
                          metadata.version
            )
            SELECT updated.room_id AS "metadata_room_id!: RoomId",
                   updated.media_id AS "metadata_media_id?: MediaId",
                   updated.playlist_id AS "metadata_playlist_id?: PlaylistId",
                   updated.target_hash AS "metadata_target_hash!",
                   updated.media_name AS metadata_media_name,
                   updated.playlist_name AS metadata_playlist_name,
                   updated.playback_kind AS "metadata_playback_kind!: PlaybackKind",
                   updated.duration_seconds,
                   updated.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   updated.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   updated.duration_error,
                   updated.next_retry_at,
                   updated.created_at AS "metadata_created_at!",
                   updated.updated_at AS "metadata_updated_at!",
                   updated.version AS "metadata_version!",
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS "state_target?: ProviderTarget",
                   state.current_progress_id AS state_current_progress_id,
                   state.history_cursor_id AS state_history_cursor_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS "state_speed!",
                   state.is_playing AS "state_is_playing!",
                   state.playback_generation AS "state_playback_generation!",
                   state.updated_at AS "state_updated_at!",
                   state.version AS "state_version!"
            FROM updated
            JOIN room_playback_state state
              ON state.room_id = updated.room_id
             AND state.playing_media_id IS NOT DISTINCT FROM updated.media_id
             AND state.playing_playlist_id IS NOT DISTINCT FROM updated.playlist_id
            JOIN room_playback_progress progress
              ON progress.id = state.current_progress_id
             AND progress.target_hash = updated.target_hash
            "#,
            &claimable_initial_statuses[..],
            i16::from(PlaybackDurationStatus::Pending),
            limit,
            i16::from(PlaybackDurationStatus::Pending),
            &room_ids,
            i16::from(PlaybackKind::Regular),
        )
        .fetch_all(&self.pool)
        .await?;

        let probes = rows
            .into_iter()
            .map(|row| {
                let (metadata, state) = row.into_parts();
                ClaimedPlaybackDurationProbe { metadata, state }
            })
            .collect();

        Ok(probes)
    }

    pub async fn claim_duration_probe_for_active_source(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<Option<ClaimedPlaybackDurationProbe>> {
        let claimable_initial_statuses = PlaybackDurationStatus::claimable_initial_statuses();
        let row = sqlx::query_as!(
            PlaybackSourceWithStateRow,
            r#"
            WITH claimed AS (
                SELECT metadata.room_id,
                       metadata.media_id,
                       metadata.playlist_id,
                       metadata.target_hash
                FROM playback_source_metadata metadata
                JOIN room_playback_state state
                  ON state.room_id = metadata.room_id
                 AND state.playing_media_id IS NOT DISTINCT FROM metadata.media_id
                 AND state.playing_playlist_id IS NOT DISTINCT FROM metadata.playlist_id
                JOIN room_playback_progress progress
                  ON progress.id = state.current_progress_id
                 AND progress.target_hash = metadata.target_hash
                WHERE metadata.room_id = $1
                  AND metadata.media_id IS NOT DISTINCT FROM $2
                  AND metadata.playlist_id IS NOT DISTINCT FROM $3
                  AND metadata.target_hash = $4
                  AND (
                      metadata.duration_status = ANY($5::smallint[])
                      OR (
                          metadata.duration_status = $6
                          AND metadata.next_retry_at <= NOW()
                      )
                  )
                  AND state.is_playing = TRUE
                  AND metadata.playback_kind = $8
                  AND metadata.duration_seconds IS NULL
                  AND (metadata.next_retry_at IS NULL OR metadata.next_retry_at <= NOW())
                LIMIT 1
                FOR UPDATE OF metadata SKIP LOCKED
            ),
            updated AS (
                UPDATE playback_source_metadata metadata
                   SET duration_status = $7,
                       duration_error = NULL,
                       next_retry_at = NOW() + INTERVAL '5 minutes',
                       version = metadata.version + 1
                  FROM claimed
                 WHERE metadata.room_id = claimed.room_id
                   AND metadata.media_id IS NOT DISTINCT FROM claimed.media_id
                   AND metadata.playlist_id IS NOT DISTINCT FROM claimed.playlist_id
                   AND metadata.target_hash = claimed.target_hash
                RETURNING metadata.room_id,
                          metadata.media_id,
                          metadata.playlist_id,
                          metadata.target_hash,
                          metadata.media_name,
                          metadata.playlist_name,
                          metadata.playback_kind,
                          metadata.duration_seconds,
                          metadata.duration_status,
                          metadata.duration_source,
                          metadata.duration_error,
                          metadata.next_retry_at,
                          metadata.created_at,
                          metadata.updated_at,
                          metadata.version
            )
            SELECT updated.room_id AS "metadata_room_id!: RoomId",
                   updated.media_id AS "metadata_media_id?: MediaId",
                   updated.playlist_id AS "metadata_playlist_id?: PlaylistId",
                   updated.target_hash AS "metadata_target_hash!",
                   updated.media_name AS metadata_media_name,
                   updated.playlist_name AS metadata_playlist_name,
                   updated.playback_kind AS "metadata_playback_kind!: PlaybackKind",
                   updated.duration_seconds,
                   updated.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   updated.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   updated.duration_error,
                   updated.next_retry_at,
                   updated.created_at AS "metadata_created_at!",
                   updated.updated_at AS "metadata_updated_at!",
                   updated.version AS "metadata_version!",
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS "state_target?: ProviderTarget",
                   state.current_progress_id AS state_current_progress_id,
                   state.history_cursor_id AS state_history_cursor_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS "state_speed!",
                   state.is_playing AS "state_is_playing!",
                   state.playback_generation AS "state_playback_generation!",
                   state.updated_at AS "state_updated_at!",
                   state.version AS "state_version!"
            FROM updated
            JOIN room_playback_state state
              ON state.room_id = updated.room_id
             AND state.playing_media_id IS NOT DISTINCT FROM updated.media_id
             AND state.playing_playlist_id IS NOT DISTINCT FROM updated.playlist_id
            JOIN room_playback_progress progress
              ON progress.id = state.current_progress_id
             AND progress.target_hash = updated.target_hash
            "#,
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            &claimable_initial_statuses[..],
            i16::from(PlaybackDurationStatus::Pending),
            i16::from(PlaybackDurationStatus::Pending),
            i16::from(PlaybackKind::Regular),
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| {
            let (metadata, state) = row.into_parts();
            ClaimedPlaybackDurationProbe { metadata, state }
        }))
    }

    pub async fn complete_probe_duration(
        &self,
        identity: &PlaybackSourceIdentity,
        expected_version: i64,
        duration_seconds: f64,
    ) -> Result<bool> {
        let result = sqlx::query!(
            r"
            UPDATE playback_source_metadata
               SET duration_seconds = $5,
                   duration_status = $6,
                   duration_source = $7,
                   duration_error = NULL,
                   next_retry_at = NULL,
                   version = version + 1
             WHERE room_id = $1
               AND media_id IS NOT DISTINCT FROM $2
               AND playlist_id IS NOT DISTINCT FROM $3
               AND target_hash = $4
               AND version = $8
            ",
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            duration_seconds,
            i16::from(PlaybackDurationStatus::Available),
            i16::from(PlaybackDurationSource::Probe),
            expected_version,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }

    pub async fn mark_probe_failed(
        &self,
        identity: &PlaybackSourceIdentity,
        expected_version: i64,
        status: PlaybackDurationStatus,
        error: &str,
        retry_after: chrono::Duration,
    ) -> Result<bool> {
        let next_retry_at = crate::SystemClock.now() + retry_after;
        let error = error.chars().take(500).collect::<String>();
        let result = sqlx::query!(
            r"
            UPDATE playback_source_metadata
               SET duration_status = $5,
                   duration_error = $6,
                   next_retry_at = $7,
                   version = version + 1
             WHERE room_id = $1
               AND media_id IS NOT DISTINCT FROM $2
               AND playlist_id IS NOT DISTINCT FROM $3
               AND target_hash = $4
               AND version = $8
            ",
            identity.room_id as RoomId,
            identity.media_id.map(i64::from),
            identity.playlist_id.map(i64::from),
            &identity.target_hash,
            i16::from(status),
            error,
            next_retry_at,
            expected_version,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }
}
