use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::{
    models::{
        try_hash_playback_target, MediaId, PlaybackHistoryEntry, PlaybackHistoryPage, PlaylistId,
        ProviderTarget, RoomId, UserId,
    },
    Error, Result,
};

#[derive(Clone, Debug)]
pub struct PlaybackHistoryRepository {
    pool: PgPool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackHistoryDirection {
    Previous,
    Next,
}

pub struct AppendPlaybackHistoryEntry<'a> {
    pub room_id: &'a RoomId,
    pub media_id: Option<MediaId>,
    pub playlist_id: Option<PlaylistId>,
    pub target: Option<&'a ProviderTarget>,
    pub position_seconds: f64,
    pub selected_by_user_id: Option<UserId>,
    pub media_name: Option<&'a str>,
    pub playlist_name: Option<&'a str>,
}

#[derive(sqlx::FromRow)]
struct PlaybackHistoryRow {
    id: i64,
    room_id: RoomId,
    sequence: i64,
    media_id: Option<MediaId>,
    playlist_id: Option<PlaylistId>,
    target: Option<ProviderTarget>,
    position_seconds: f64,
    selected_by_user_id: Option<UserId>,
    #[sqlx(default)]
    media_name: Option<String>,
    playlist_name: Option<String>,
    #[sqlx(default)]
    source_provider: Option<crate::models::SourceProvider>,
    #[sqlx(default)]
    provider_instance_name: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PlaybackHistoryRow> for PlaybackHistoryEntry {
    fn from(row: PlaybackHistoryRow) -> Self {
        Self {
            id: row.id,
            room_id: row.room_id,
            sequence: row.sequence,
            media_id: row.media_id,
            playlist_id: row.playlist_id,
            target: row.target,
            position_seconds: row.position_seconds,
            selected_by_user_id: row.selected_by_user_id,
            media_name: row.media_name,
            playlist_name: row.playlist_name,
            source_provider: row.source_provider,
            provider_instance_name: row.provider_instance_name,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl PlaybackHistoryRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(
        &self,
        room_id: &RoomId,
        before_entry_id: Option<i64>,
        limit: i32,
    ) -> Result<PlaybackHistoryPage> {
        let limit = limit.clamp(1, 100);
        let rows = sqlx::query_as!(
            PlaybackHistoryRow,
            r#"SELECT history.id AS "id!", history.room_id AS "room_id!: RoomId", history.sequence AS "sequence!",
                      history.media_id AS "media_id?: MediaId", history.playlist_id AS "playlist_id?: PlaylistId",
                      history.target AS "target?: crate::models::ProviderTarget",
                      history.position_seconds, history.selected_by_user_id AS "selected_by_user_id?: UserId",
                      history.media_name,
                      history.playlist_name,
                      COALESCE(media.source_provider, playlist.source_provider) AS "source_provider?: crate::models::SourceProvider",
                      COALESCE(media.provider_instance_name, playlist.provider_instance_name)
                          AS "provider_instance_name?",
                      history.created_at, history.updated_at
               FROM room_playback_history history
               LEFT JOIN media
                 ON media.id = history.media_id AND media.room_id = history.room_id
               LEFT JOIN playlists playlist
                 ON playlist.id = history.playlist_id AND playlist.room_id = history.room_id
               WHERE history.room_id = $1
                 AND ($2::bigint IS NULL OR history.id < $2)
               ORDER BY history.sequence DESC
               LIMIT $3"#,
            room_id.as_i64(),
            before_entry_id,
            i64::from(limit) + 1,
        )
        .fetch_all(&self.pool)
        .await?;
        let limit = usize::try_from(limit).map_err(|error| {
            Error::Internal(format!(
                "validated playback history limit is invalid: {error}"
            ))
        })?;
        let has_more = rows.len() > limit;
        let entries = rows
            .into_iter()
            .take(limit)
            .map(PlaybackHistoryEntry::from)
            .collect::<Vec<_>>();
        let history_cursor_id = sqlx::query_scalar!(
            r#"SELECT history_cursor_id AS "history_cursor_id?"
               FROM room_playback_state WHERE room_id = $1"#,
            room_id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        let next_before_entry_id = has_more.then(|| entries.last().expect("non-empty page").id);
        Ok(PlaybackHistoryPage {
            entries,
            history_cursor_id,
            next_before_entry_id,
        })
    }

    pub async fn cursor_entry(&self, room_id: &RoomId) -> Result<Option<PlaybackHistoryEntry>> {
        let mut conn = self.pool.acquire().await?;
        self.cursor_entry_on_conn(room_id, &mut conn).await
    }

    pub async fn adjacent_entry(
        &self,
        room_id: &RoomId,
        entry_id: i64,
        direction: PlaybackHistoryDirection,
    ) -> Result<Option<PlaybackHistoryEntry>> {
        let mut conn = self.pool.acquire().await?;
        self.adjacent_entry_on_conn(room_id, entry_id, direction, &mut conn)
            .await
    }

    pub async fn cursor_entry_on_conn(
        &self,
        room_id: &RoomId,
        conn: &mut PgConnection,
    ) -> Result<Option<PlaybackHistoryEntry>> {
        let row = sqlx::query_as!(
            PlaybackHistoryRow,
            r#"SELECT h.id AS "id!", h.room_id AS "room_id!: RoomId", h.sequence AS "sequence!",
                      h.media_id AS "media_id?: MediaId", h.playlist_id AS "playlist_id?: PlaylistId",
                      h.target AS "target?: crate::models::ProviderTarget",
                      h.media_name, h.playlist_name, h.position_seconds,
                      h.selected_by_user_id AS "selected_by_user_id?: UserId", h.created_at, h.updated_at
                      ,NULL::smallint AS "source_provider?: crate::models::SourceProvider"
                      ,NULL::text AS "provider_instance_name?"
               FROM room_playback_state c
               JOIN room_playback_history h
                 ON h.room_id = c.room_id
                AND h.id = c.history_cursor_id
                AND h.created_at = c.history_cursor_created_at
               WHERE c.room_id = $1
               FOR UPDATE OF c"#,
            room_id.as_i64(),
        )
        .fetch_optional(&mut *conn)
        .await?;
        Ok(row.map(PlaybackHistoryEntry::from))
    }

    pub async fn adjacent_entry_on_conn(
        &self,
        room_id: &RoomId,
        entry_id: i64,
        direction: PlaybackHistoryDirection,
        conn: &mut PgConnection,
    ) -> Result<Option<PlaybackHistoryEntry>> {
        let row = match direction {
            PlaybackHistoryDirection::Next => {
                sqlx::query_as!(
                    PlaybackHistoryRow,
                    r#"SELECT candidate.id AS "id!", candidate.room_id AS "room_id!: RoomId", candidate.sequence AS "sequence!",
                      candidate.media_id AS "media_id?: MediaId", candidate.playlist_id AS "playlist_id?: PlaylistId",
                      candidate.target AS "target?: crate::models::ProviderTarget",
                      candidate.media_name, candidate.playlist_name, candidate.position_seconds,
                      candidate.selected_by_user_id AS "selected_by_user_id?: UserId", candidate.created_at,
                      candidate.updated_at,
                      NULL::smallint AS "source_provider?: crate::models::SourceProvider",
                      NULL::text AS "provider_instance_name?"
               FROM room_playback_history current
               JOIN LATERAL (
                   SELECT h.* FROM room_playback_history h
                   WHERE h.room_id = current.room_id AND h.sequence > current.sequence
                   ORDER BY h.sequence ASC LIMIT 1
               ) candidate ON TRUE
               WHERE current.room_id = $1 AND current.id = $2"#,
                    room_id.as_i64(),
                    entry_id,
                )
                .fetch_optional(&mut *conn)
                .await?
            }
            PlaybackHistoryDirection::Previous => {
                sqlx::query_as!(
                    PlaybackHistoryRow,
                    r#"SELECT candidate.id AS "id!", candidate.room_id AS "room_id!: RoomId", candidate.sequence AS "sequence!",
                      candidate.media_id AS "media_id?: MediaId", candidate.playlist_id AS "playlist_id?: PlaylistId",
                      candidate.target AS "target?: crate::models::ProviderTarget",
                      candidate.media_name, candidate.playlist_name, candidate.position_seconds,
                      candidate.selected_by_user_id AS "selected_by_user_id?: UserId", candidate.created_at,
                      candidate.updated_at,
                      NULL::smallint AS "source_provider?: crate::models::SourceProvider",
                      NULL::text AS "provider_instance_name?"
               FROM room_playback_history current
               JOIN LATERAL (
                   SELECT h.* FROM room_playback_history h
                   WHERE h.room_id = current.room_id AND h.sequence < current.sequence
                   ORDER BY h.sequence DESC LIMIT 1
               ) candidate ON TRUE
               WHERE current.room_id = $1 AND current.id = $2"#,
                    room_id.as_i64(),
                    entry_id,
                )
                .fetch_optional(&mut *conn)
                .await?
            }
        };
        Ok(row.map(PlaybackHistoryEntry::from))
    }

    pub async fn get_on_conn(
        &self,
        room_id: &RoomId,
        entry_id: i64,
        conn: &mut PgConnection,
    ) -> Result<PlaybackHistoryEntry> {
        sqlx::query_as!(
            PlaybackHistoryRow,
            r#"SELECT id AS "id!", room_id AS "room_id!: RoomId", sequence AS "sequence!",
                      media_id AS "media_id?: MediaId", playlist_id AS "playlist_id?: PlaylistId",
                      target AS "target?: crate::models::ProviderTarget",
                      media_name, playlist_name, position_seconds, selected_by_user_id AS "selected_by_user_id?: UserId", created_at, updated_at
                      ,NULL::smallint AS "source_provider?: crate::models::SourceProvider"
                      ,NULL::text AS "provider_instance_name?"
               FROM room_playback_history WHERE room_id = $1 AND id = $2"#,
            room_id.as_i64(),
            entry_id,
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(PlaybackHistoryEntry::from)
        .ok_or_else(|| Error::NotFound("Playback history entry not found".to_string()))
    }

    pub async fn save_cursor_position_on_conn(
        &self,
        room_id: &RoomId,
        position_seconds: f64,
        conn: &mut PgConnection,
    ) -> Result<()> {
        sqlx::query!(
            r#"UPDATE room_playback_history h SET position_seconds = $2
               FROM room_playback_state c
               WHERE c.room_id = $1
                 AND h.room_id = c.room_id
                 AND h.id = c.history_cursor_id
                 AND h.created_at = c.history_cursor_created_at"#,
            room_id.as_i64(),
            position_seconds.max(0.0),
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    pub async fn append_entry_on_conn(
        &self,
        request: AppendPlaybackHistoryEntry<'_>,
        conn: &mut PgConnection,
    ) -> Result<PlaybackHistoryEntry> {
        let AppendPlaybackHistoryEntry {
            room_id,
            media_id,
            playlist_id,
            target,
            position_seconds,
            selected_by_user_id,
            media_name,
            playlist_name,
        } = request;
        let cursor_entry = self.cursor_entry_on_conn(room_id, conn).await?;
        if let Some(cursor_entry) = &cursor_entry {
            sqlx::query!(
                "DELETE FROM room_playback_history WHERE room_id = $1 AND sequence > $2",
                room_id.as_i64(),
                cursor_entry.sequence,
            )
            .execute(&mut *conn)
            .await?;
        }
        let next_sequence = match cursor_entry.as_ref() {
            Some(entry) => entry.sequence + 1,
            None => {
                sqlx::query_scalar!(
                    r#"SELECT (COALESCE(MAX(sequence), 0) + 1)::BIGINT AS "next_sequence!"
                       FROM room_playback_history WHERE room_id = $1"#,
                    room_id.as_i64(),
                )
                .fetch_one(&mut *conn)
                .await?
            }
        };
        let target_hash = try_hash_playback_target(target)?;
        let row = sqlx::query_as!(
            PlaybackHistoryRow,
            r#"INSERT INTO room_playback_history (
                   room_id, sequence, media_id, playlist_id, target, target_hash,
                   media_name, playlist_name, position_seconds, selected_by_user_id
               ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
               RETURNING id AS "id!", room_id AS "room_id!: RoomId", sequence AS "sequence!",
                         media_id AS "media_id?: MediaId", playlist_id AS "playlist_id?: PlaylistId",
                         target AS "target?: crate::models::ProviderTarget",
                         media_name, playlist_name, position_seconds, selected_by_user_id AS "selected_by_user_id?: UserId", created_at, updated_at,
                         NULL::smallint AS "source_provider?: crate::models::SourceProvider",
                         NULL::text AS "provider_instance_name?""#,
            room_id.as_i64(),
            next_sequence,
            media_id.map(|id| id.as_i64()),
            playlist_id.map(|id| id.as_i64()),
            target.map(serde_json::to_value).transpose()?,
            target_hash,
            media_name,
            playlist_name,
            position_seconds.max(0.0),
            selected_by_user_id.map(|id| id.as_i64()),
        )
        .fetch_one(&mut *conn)
        .await?;
        let entry = PlaybackHistoryEntry::from(row);
        self.set_cursor_on_conn(room_id, &entry, conn).await?;
        Ok(entry)
    }

    pub async fn set_cursor_on_conn(
        &self,
        room_id: &RoomId,
        entry: &PlaybackHistoryEntry,
        conn: &mut PgConnection,
    ) -> Result<()> {
        sqlx::query!(
            r#"UPDATE room_playback_state
               SET history_cursor_id = $2, history_cursor_created_at = $3
               WHERE room_id = $1"#,
            room_id.as_i64(),
            entry.id,
            entry.created_at,
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    pub async fn cleanup(&self, retention_days: u32, max_entries_per_room: i64) -> Result<u64> {
        if retention_days == 0 && max_entries_per_room <= 0 {
            return Ok(0);
        }
        let retention_days = i32::try_from(retention_days).map_err(|_| {
            Error::InvalidInput("playback history retention days is too large".to_string())
        })?;
        let max_entries_per_room = max_entries_per_room.max(0);
        let mut deleted_total = 0_u64;
        loop {
            let result = sqlx::query!(
                r#"WITH ranked AS (
                   SELECT h.room_id, h.id, h.sequence, h.created_at,
                          ROW_NUMBER() OVER (PARTITION BY h.room_id ORDER BY h.sequence DESC) AS rank
                   FROM room_playback_history h
               ), candidate_rooms AS (
                   SELECT r.room_id
                   FROM ranked r
                   WHERE (($1 > 0 AND r.created_at < CURRENT_TIMESTAMP - make_interval(days => $1))
                       OR ($2::BIGINT > 0 AND r.rank > $2::BIGINT))
                   GROUP BY r.room_id
                   ORDER BY MIN(r.created_at), r.room_id
               ), locked_cursors AS MATERIALIZED (
                   SELECT c.room_id, c.history_cursor_id, c.history_cursor_created_at
                   FROM room_playback_state c
                   JOIN candidate_rooms rooms ON rooms.room_id = c.room_id
                   FOR UPDATE OF c
               ), protected AS (
                   SELECT c.room_id, c.history_cursor_id AS id
                   FROM locked_cursors c
                   WHERE c.history_cursor_id IS NOT NULL
                   UNION
                   SELECT c.room_id, neighbor.id
                   FROM locked_cursors c
                   JOIN room_playback_history current
                     ON current.room_id = c.room_id
                    AND current.id = c.history_cursor_id
                    AND current.created_at = c.history_cursor_created_at
                   JOIN LATERAL (
                       (SELECT h.id FROM room_playback_history h
                        WHERE h.room_id = current.room_id AND h.sequence < current.sequence
                        ORDER BY h.sequence DESC LIMIT 1)
                       UNION ALL
                       (SELECT h.id FROM room_playback_history h
                        WHERE h.room_id = current.room_id AND h.sequence > current.sequence
                        ORDER BY h.sequence ASC LIMIT 1)
                   ) neighbor ON TRUE
               ), candidates AS (
                   SELECT r.room_id, r.id, r.created_at
                   FROM ranked r
                   JOIN candidate_rooms rooms ON rooms.room_id = r.room_id
                   WHERE (($1 > 0 AND r.created_at < CURRENT_TIMESTAMP - make_interval(days => $1))
                       OR ($2::BIGINT > 0 AND r.rank > $2::BIGINT))
                     AND NOT EXISTS (
                         SELECT 1 FROM protected p WHERE p.room_id = r.room_id AND p.id = r.id
                     )
                   ORDER BY r.created_at ASC, r.room_id, r.id
                   LIMIT 5000
               )
               DELETE FROM room_playback_history h
               USING candidates c
               WHERE h.room_id = c.room_id AND h.id = c.id AND h.created_at = c.created_at"#,
                retention_days,
                max_entries_per_room,
            )
            .execute(&self.pool)
            .await?;
            let deleted = result.rows_affected();
            deleted_total = deleted_total.saturating_add(deleted);
            if deleted < 5_000 {
                return Ok(deleted_total);
            }
        }
    }
}
