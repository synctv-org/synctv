use sqlx::{PgPool, Row as _};

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

impl PlaybackSourceMetadataRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<Option<PlaybackSourceMetadata>> {
        let metadata = sqlx::query_as::<_, PlaybackSourceMetadata>(
            r"
            SELECT room_id,
                   media_id,
                   playlist_id,
                   target_hash,
                   duration_seconds,
                   duration_status,
                   duration_source,
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
            ",
        )
        .bind(identity.room_id)
        .bind(identity.media_id)
        .bind(identity.playlist_id)
        .bind(&identity.target_hash)
        .fetch_optional(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn upsert_provider_duration(
        &self,
        identity: &PlaybackSourceIdentity,
        duration_seconds: f64,
    ) -> Result<PlaybackSourceMetadata> {
        let metadata = sqlx::query_as::<_, PlaybackSourceMetadata>(
            r"
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
            RETURNING room_id,
                      media_id,
                      playlist_id,
                      target_hash,
                      duration_seconds,
                      duration_status,
                      duration_source,
                      duration_error,
                      next_retry_at,
                      created_at,
                      updated_at,
                      version
            ",
        )
        .bind(identity.room_id)
        .bind(identity.media_id)
        .bind(identity.playlist_id)
        .bind(&identity.target_hash)
        .bind(duration_seconds)
        .bind(i16::from(PlaybackDurationStatus::Available))
        .bind(i16::from(PlaybackDurationSource::Provider))
        .fetch_one(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn mark_unknown_if_absent(
        &self,
        identity: &PlaybackSourceIdentity,
    ) -> Result<PlaybackSourceMetadata> {
        let metadata = sqlx::query_as::<_, PlaybackSourceMetadata>(
            r"
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
            RETURNING room_id,
                      media_id,
                      playlist_id,
                      target_hash,
                      duration_seconds,
                      duration_status,
                      duration_source,
                      duration_error,
                      next_retry_at,
                      created_at,
                      updated_at,
                      version
            ",
        )
        .bind(identity.room_id)
        .bind(identity.media_id)
        .bind(identity.playlist_id)
        .bind(&identity.target_hash)
        .bind(i16::from(PlaybackDurationStatus::Unknown))
        .fetch_one(&self.pool)
        .await?;

        Ok(metadata)
    }

    pub async fn list_active_finite_sources(
        &self,
        limit: i64,
    ) -> Result<Vec<(PlaybackSourceMetadata, RoomPlaybackState)>> {
        let rows = sqlx::query(
            r#"
            SELECT metadata.room_id AS metadata_room_id,
                   metadata.media_id AS metadata_media_id,
                   metadata.playlist_id AS metadata_playlist_id,
                   metadata.target_hash AS metadata_target_hash,
                   metadata.duration_seconds,
                   metadata.duration_status AS metadata_duration_status,
                   metadata.duration_source AS metadata_duration_source,
                   metadata.duration_error,
                   metadata.next_retry_at,
                   metadata.created_at AS metadata_created_at,
                   metadata.updated_at AS metadata_updated_at,
                   metadata.version AS metadata_version,
                   state.room_id AS state_room_id,
                   state.playing_media_id AS state_playing_media_id,
                   state.playing_playlist_id AS state_playing_playlist_id,
                   state.target AS state_target,
                   state.current_progress_id AS state_current_progress_id,
                   COALESCE(progress."position", 0.0) AS state_position,
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
              AND metadata.duration_status = $1
              AND metadata.duration_seconds IS NOT NULL
            ORDER BY state.updated_at ASC
            LIMIT $2
            "#,
        )
        .bind(i16::from(PlaybackDurationStatus::Available))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let sources = rows
            .into_iter()
            .map(|row| {
                let metadata = PlaybackSourceMetadata {
                    room_id: row.get::<RoomId, _>("metadata_room_id"),
                    media_id: row.get::<Option<MediaId>, _>("metadata_media_id"),
                    playlist_id: row.get::<Option<PlaylistId>, _>("metadata_playlist_id"),
                    target_hash: row.get("metadata_target_hash"),
                    duration_seconds: row.get("duration_seconds"),
                    duration_status: row.get("metadata_duration_status"),
                    duration_source: row.get("metadata_duration_source"),
                    duration_error: row.get("duration_error"),
                    next_retry_at: row.get("next_retry_at"),
                    created_at: row.get("metadata_created_at"),
                    updated_at: row.get("metadata_updated_at"),
                    version: row.get("metadata_version"),
                };
                let state = RoomPlaybackState {
                    room_id: row.get("state_room_id"),
                    playing_media_id: row.get("state_playing_media_id"),
                    playing_playlist_id: row.get("state_playing_playlist_id"),
                    target: row.get("state_target"),
                    current_progress_id: row.get("state_current_progress_id"),
                    position: row.get("state_position"),
                    speed: row.get("state_speed"),
                    is_playing: row.get("state_is_playing"),
                    updated_at: row.get("state_updated_at"),
                    version: row.get("state_version"),
                };
                (metadata, state)
            })
            .collect();

        Ok(sources)
    }

    pub async fn claim_duration_probe_batch(
        &self,
        limit: i64,
    ) -> Result<Vec<ClaimedPlaybackDurationProbe>> {
        let rows = sqlx::query(
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
                WHERE (
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
            SELECT updated.room_id AS metadata_room_id,
                   updated.media_id AS metadata_media_id,
                   updated.playlist_id AS metadata_playlist_id,
                   updated.target_hash AS metadata_target_hash,
                   updated.duration_seconds,
                   updated.duration_status AS metadata_duration_status,
                   updated.duration_source AS metadata_duration_source,
                   updated.duration_error,
                   updated.next_retry_at,
                   updated.created_at AS metadata_created_at,
                   updated.updated_at AS metadata_updated_at,
                   updated.version AS metadata_version,
                   state.room_id AS state_room_id,
                   state.playing_media_id AS state_playing_media_id,
                   state.playing_playlist_id AS state_playing_playlist_id,
                   state.target AS state_target,
                   state.current_progress_id AS state_current_progress_id,
                   COALESCE(progress."position", 0.0) AS state_position,
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
        )
        .bind(i16::from(PlaybackDurationStatus::Unknown))
        .bind(i16::from(PlaybackDurationStatus::Failed))
        .bind(i16::from(PlaybackDurationStatus::Unavailable))
        .bind(i16::from(PlaybackDurationStatus::Pending))
        .bind(limit)
        .bind(i16::from(PlaybackDurationStatus::Pending))
        .fetch_all(&self.pool)
        .await?;

        let probes = rows
            .into_iter()
            .map(|row| {
                let metadata = PlaybackSourceMetadata {
                    room_id: row.get::<RoomId, _>("metadata_room_id"),
                    media_id: row.get::<Option<MediaId>, _>("metadata_media_id"),
                    playlist_id: row.get::<Option<PlaylistId>, _>("metadata_playlist_id"),
                    target_hash: row.get("metadata_target_hash"),
                    duration_seconds: row.get("duration_seconds"),
                    duration_status: row.get("metadata_duration_status"),
                    duration_source: row.get("metadata_duration_source"),
                    duration_error: row.get("duration_error"),
                    next_retry_at: row.get("next_retry_at"),
                    created_at: row.get("metadata_created_at"),
                    updated_at: row.get("metadata_updated_at"),
                    version: row.get("metadata_version"),
                };
                let state = RoomPlaybackState {
                    room_id: row.get("state_room_id"),
                    playing_media_id: row.get("state_playing_media_id"),
                    playing_playlist_id: row.get("state_playing_playlist_id"),
                    target: row.get("state_target"),
                    current_progress_id: row.get("state_current_progress_id"),
                    position: row.get("state_position"),
                    speed: row.get("state_speed"),
                    is_playing: row.get("state_is_playing"),
                    updated_at: row.get("state_updated_at"),
                    version: row.get("state_version"),
                };
                ClaimedPlaybackDurationProbe { metadata, state }
            })
            .collect();

        Ok(probes)
    }

    pub async fn complete_probe_duration(
        &self,
        identity: &PlaybackSourceIdentity,
        expected_version: i64,
        duration_seconds: f64,
    ) -> Result<bool> {
        let result = sqlx::query(
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
        )
        .bind(identity.room_id)
        .bind(identity.media_id)
        .bind(identity.playlist_id)
        .bind(&identity.target_hash)
        .bind(duration_seconds)
        .bind(i16::from(PlaybackDurationStatus::Available))
        .bind(i16::from(PlaybackDurationSource::Probe))
        .bind(expected_version)
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
        let result = sqlx::query(
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
        )
        .bind(identity.room_id)
        .bind(identity.media_id)
        .bind(identity.playlist_id)
        .bind(&identity.target_hash)
        .bind(i16::from(status))
        .bind(error.chars().take(500).collect::<String>())
        .bind(next_retry_at)
        .bind(expected_version)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() == 1)
    }
}
