use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{
    models::{
        ClaimedPlaybackDurationProbe, MediaId, PlaybackDurationSource, PlaybackDurationStatus,
        PlaybackSourceIdentity, PlaybackSourceMetadata, PlaylistId, RoomId, RoomPlaybackState,
    },
    Result,
};

#[derive(Clone)]
pub struct PlaybackSourceMetadataRepository {
    pool: PgPool,
}

#[derive(Debug)]
struct PlaybackSourceWithStateRow {
    metadata_room_id: RoomId,
    metadata_media_id: Option<MediaId>,
    metadata_playlist_id: Option<PlaylistId>,
    metadata_target_hash: String,
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
    state_target: Vec<u8>,
    state_current_progress_id: Option<i64>,
    state_position: f64,
    state_speed: f64,
    state_is_playing: bool,
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
            position: self.state_position,
            speed: self.state_speed,
            is_playing: self.state_is_playing,
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

    pub async fn upsert_provider_duration(
        &self,
        identity: &PlaybackSourceIdentity,
        duration_seconds: f64,
    ) -> Result<PlaybackSourceMetadata> {
        let metadata = sqlx::query_as!(
            PlaybackSourceMetadata,
            r#"
            INSERT INTO playback_source_metadata (
                room_id,
                media_id,
                playlist_id,
                target_hash,
                duration_seconds,
                duration_status,
                duration_source,
                duration_error,
                next_retry_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, NULL, NULL)
            ON CONFLICT (
                room_id,
                COALESCE(media_id, 0),
                COALESCE(playlist_id, 0),
                target_hash
            )
            DO UPDATE SET duration_seconds = EXCLUDED.duration_seconds,
                          duration_status = EXCLUDED.duration_status,
                          duration_source = EXCLUDED.duration_source,
                          duration_error = NULL,
                          next_retry_at = NULL,
                          version = playback_source_metadata.version + 1
            RETURNING room_id AS "room_id!: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target_hash,
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
            duration_seconds,
            i16::from(PlaybackDurationStatus::Available),
            i16::from(PlaybackDurationSource::Provider),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn mark_unknown_if_absent(
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
                duration_status
            )
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (
                room_id,
                COALESCE(media_id, 0),
                COALESCE(playlist_id, 0),
                target_hash
            )
            DO UPDATE SET target_hash = playback_source_metadata.target_hash
            RETURNING room_id AS "room_id!: RoomId",
                      media_id AS "media_id?: MediaId",
                      playlist_id AS "playlist_id?: PlaylistId",
                      target_hash,
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
            i16::from(PlaybackDurationStatus::Unknown),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(metadata)
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
                   metadata.target_hash AS metadata_target_hash,
                   metadata.duration_seconds,
                   metadata.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   metadata.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   metadata.duration_error,
                   metadata.next_retry_at,
                   metadata.created_at AS metadata_created_at,
                   metadata.updated_at AS metadata_updated_at,
                   metadata.version AS metadata_version,
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS state_target,
                   state.current_progress_id AS state_current_progress_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS state_speed,
                   state.is_playing AS state_is_playing,
                   state.updated_at AS state_updated_at,
                   state.version AS state_version
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
                WHERE state.room_id = ANY($7)
                  AND (
                      metadata.duration_status IN ($1, $2, $3)
                      OR (
                          metadata.duration_status = $4
                          AND metadata.next_retry_at <= NOW()
                      )
                )
                  AND state.is_playing = TRUE
                  AND metadata.duration_seconds IS NULL
                  AND (metadata.next_retry_at IS NULL OR metadata.next_retry_at <= NOW())
                ORDER BY metadata.updated_at ASC
                LIMIT $5
                FOR UPDATE OF metadata SKIP LOCKED
            ),
            updated AS (
                UPDATE playback_source_metadata metadata
                   SET duration_status = $6,
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
                   updated.target_hash AS metadata_target_hash,
                   updated.duration_seconds,
                   updated.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   updated.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   updated.duration_error,
                   updated.next_retry_at,
                   updated.created_at AS metadata_created_at,
                   updated.updated_at AS metadata_updated_at,
                   updated.version AS metadata_version,
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS state_target,
                   state.current_progress_id AS state_current_progress_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS state_speed,
                   state.is_playing AS state_is_playing,
                   state.updated_at AS state_updated_at,
                   state.version AS state_version
            FROM updated
            JOIN room_playback_state state
              ON state.room_id = updated.room_id
             AND state.playing_media_id IS NOT DISTINCT FROM updated.media_id
             AND state.playing_playlist_id IS NOT DISTINCT FROM updated.playlist_id
            JOIN room_playback_progress progress
              ON progress.id = state.current_progress_id
             AND progress.target_hash = updated.target_hash
            "#,
            i16::from(PlaybackDurationStatus::Unknown),
            i16::from(PlaybackDurationStatus::Failed),
            i16::from(PlaybackDurationStatus::Unavailable),
            i16::from(PlaybackDurationStatus::Pending),
            limit,
            i16::from(PlaybackDurationStatus::Pending),
            &room_ids,
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
                      metadata.duration_status IN ($5, $6, $7)
                      OR (
                          metadata.duration_status = $8
                          AND metadata.next_retry_at <= NOW()
                      )
                  )
                  AND state.is_playing = TRUE
                  AND metadata.duration_seconds IS NULL
                  AND (metadata.next_retry_at IS NULL OR metadata.next_retry_at <= NOW())
                LIMIT 1
                FOR UPDATE OF metadata SKIP LOCKED
            ),
            updated AS (
                UPDATE playback_source_metadata metadata
                   SET duration_status = $9,
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
                   updated.target_hash AS metadata_target_hash,
                   updated.duration_seconds,
                   updated.duration_status AS "metadata_duration_status!: PlaybackDurationStatus",
                   updated.duration_source AS "metadata_duration_source?: PlaybackDurationSource",
                   updated.duration_error,
                   updated.next_retry_at,
                   updated.created_at AS metadata_created_at,
                   updated.updated_at AS metadata_updated_at,
                   updated.version AS metadata_version,
                   state.room_id AS "state_room_id!: RoomId",
                   state.playing_media_id AS "state_playing_media_id?: MediaId",
                   state.playing_playlist_id AS "state_playing_playlist_id?: PlaylistId",
                   state.target AS state_target,
                   state.current_progress_id AS state_current_progress_id,
                   COALESCE(progress."position", 0.0) AS "state_position!",
                   state.speed AS state_speed,
                   state.is_playing AS state_is_playing,
                   state.updated_at AS state_updated_at,
                   state.version AS state_version
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
            i16::from(PlaybackDurationStatus::Unknown),
            i16::from(PlaybackDurationStatus::Failed),
            i16::from(PlaybackDurationStatus::Unavailable),
            i16::from(PlaybackDurationStatus::Pending),
            i16::from(PlaybackDurationStatus::Pending),
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
        let next_retry_at = chrono::Utc::now() + retry_after;
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
