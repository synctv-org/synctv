use sqlx::{Postgres, Transaction};

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackState, UserId},
    Error, Result,
};

use super::{UserDeletedRoomImpact, UserService};

impl UserService {
    async fn collect_deleted_media_ids_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        playlist_ids: &[PlaylistId],
        media_ids: &[MediaId],
    ) -> Result<Vec<MediaId>> {
        if playlist_ids.is_empty() && media_ids.is_empty() {
            return Ok(Vec::new());
        }

        let playlist_id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
        let media_id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();

        let media_ids = sqlx::query_scalar!(
            r#"WITH RECURSIVE target_playlists AS (
                SELECT id
                FROM playlists
                WHERE id = ANY($1)
                  AND deleted_at IS NULL
                UNION ALL
                SELECT p.id
                FROM playlists p
                JOIN target_playlists tp ON p.parent_id = tp.id
                WHERE p.deleted_at IS NULL
            )
            SELECT DISTINCT m.id AS "id: MediaId"
            FROM media m
            WHERE m.room_id = $2
              AND m.deleted_at IS NULL
              AND (
                  m.id = ANY($3)
                  OR m.playlist_id IN (SELECT id FROM target_playlists)
              )
            ORDER BY m.id"#,
            &playlist_id_strs,
            room_id.as_i64(),
            &media_id_strs
        )
        .fetch_all(&mut **tx)
        .await?;

        Ok(media_ids)
    }

    pub(super) async fn delete_owned_entries_in_room_in_tx(
        &self,
        deleted_owner_id: &UserId,
        room_id: &RoomId,
        playlist_ids: Vec<PlaylistId>,
        media_ids: Vec<MediaId>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<UserDeletedRoomImpact> {
        let deleted_media_ids =
            Self::collect_deleted_media_ids_in_tx(tx, room_id, &playlist_ids, &media_ids).await?;

        let playback_row = sqlx::query!(
            r#"SELECT playing_media_id as "playing_media_id: MediaId",
                      playing_playlist_id as "playing_playlist_id: PlaylistId",
                      version
             FROM room_playback_state
             WHERE room_id = $1
             FOR UPDATE"#,
            room_id as &RoomId,
        )
        .fetch_optional(&mut **tx)
        .await?;

        let mut playback_state = None;
        let mut playback_fence = None;
        if let Some(row) = playback_row {
            let playing_media_id = row.playing_media_id;
            let playing_playlist_id = row.playing_playlist_id;
            let playback_version = row.version;
            let deletes_playing_media = playing_media_id.as_ref().is_some_and(|current_id| {
                deleted_media_ids
                    .iter()
                    .any(|media_id| media_id == current_id)
            });

            let deletes_playing_playlist = if let Some(playing_playlist_id) = playing_playlist_id {
                if playlist_ids.is_empty() {
                    false
                } else {
                    let playlist_id_strs: Vec<i64> =
                        playlist_ids.iter().map(PlaylistId::as_i64).collect();
                    sqlx::query_scalar!(
                        r#"WITH RECURSIVE target_playlists AS (
                            SELECT id
                            FROM playlists
                            WHERE id = ANY($1)
                              AND deleted_at IS NULL
                            UNION ALL
                            SELECT p.id
                            FROM playlists p
                            JOIN target_playlists tp ON p.parent_id = tp.id
                            WHERE p.deleted_at IS NULL
                        )
                        SELECT EXISTS(
                            SELECT 1
                            FROM target_playlists
                            WHERE id = $2
                        ) AS "exists!""#,
                        &playlist_id_strs,
                        &playing_playlist_id as &PlaylistId,
                    )
                    .fetch_one(&mut **tx)
                    .await?
                }
            } else {
                false
            };

            if deletes_playing_media || deletes_playing_playlist {
                let reservation = self
                    .begin_playback_reset_write(room_id, playback_version)
                    .await?;
                let reserved_version = reservation
                    .as_ref()
                    .map_or(playback_version + 1, |reservation| reservation.version);
                let reset_result: Result<RoomPlaybackState> = match sqlx::query_as!(
                    RoomPlaybackState,
                    r#"WITH current_state AS (
                            SELECT room_id, current_progress_id
                            FROM room_playback_state
                            WHERE room_id = $1 AND version = $3
                            FOR UPDATE
                        ),
                        reset_progress AS (
                            UPDATE room_playback_progress progress
                            SET "position" = 0,
                                version = version + 1
                            FROM current_state
                            WHERE progress.id = current_state.current_progress_id
                            RETURNING progress.id
                        ),
                        updated AS (
                            UPDATE room_playback_state state
                            SET playing_media_id = NULL,
                                playing_playlist_id = NULL,
                                target = NULL,
                                current_progress_id = NULL,
                                speed = 1.0,
                                is_playing = false,
                                playback_generation = playback_generation + 1,
                                version = $2,
                                updated_at = NOW()
                            FROM current_state
                            WHERE state.room_id = current_state.room_id
                            RETURNING state.room_id,
                                      state.playing_media_id,
                                      state.playing_playlist_id,
                                      state.target,
                                      state.current_progress_id,
                                      state.history_cursor_id,
                                      state.speed,
                                      state.is_playing,
                                      state.playback_generation,
                                      state.updated_at,
                                      state.version
                        )
                        SELECT room_id AS "room_id!: RoomId",
                               playing_media_id AS "playing_media_id?: MediaId",
                               playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                               target AS "target?: crate::models::ProviderTarget",
                               current_progress_id,
                               history_cursor_id,
                               0.0::DOUBLE PRECISION AS "position!",
                               speed AS "speed!",
                               is_playing AS "is_playing!",
                               playback_generation AS "playback_generation!",
                               updated_at AS "updated_at!",
                               version AS "version!"
                        FROM updated"#,
                    room_id.as_i64(),
                    reserved_version,
                    playback_version,
                )
                .fetch_optional(&mut **tx)
                .await
                {
                    Ok(Some(state)) => Ok(state),
                    Ok(None) => Err(Error::OptimisticLockConflict),
                    Err(error) => Err(error.into()),
                };
                playback_state = Some(match reset_result {
                    Ok(state) => state,
                    Err(error) => {
                        self.abort_playback_reset_fence(room_id, reservation.as_ref())
                            .await;
                        return Err(error);
                    }
                });
                playback_fence = Some(super::PendingPlaybackResetFence {
                    room_id: *room_id,
                    reservation,
                });
            }
        }

        if !media_ids.is_empty() {
            let media_id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
            if let Err(error) = sqlx::query!(
                "DELETE FROM room_playback_progress WHERE room_id = $1 AND media_id = ANY($2)",
                room_id as &RoomId,
                &media_id_strs,
            )
            .execute(&mut **tx)
            .await
            {
                self.abort_playback_reset_fence_option(playback_fence.as_ref())
                    .await;
                return Err(error.into());
            }
            if let Err(error) = sqlx::query!(
                "UPDATE media SET deleted_at = COALESCE(deleted_at, CURRENT_TIMESTAMP), deletion_source = COALESCE(deletion_source, 'account'), deleted_owner_id = COALESCE(deleted_owner_id, $2), version = version + 1 WHERE id = ANY($1) AND deleted_at IS NULL",
                &media_id_strs,
                deleted_owner_id.as_i64(),
            )
            .execute(&mut **tx)
            .await
            {
                self.abort_playback_reset_fence_option(playback_fence.as_ref())
                    .await;
                return Err(error.into());
            }
        }

        if !playlist_ids.is_empty() {
            let playlist_id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
            if let Err(error) = sqlx::query!(
                "DELETE FROM room_playback_progress WHERE room_id = $1 AND playlist_id = ANY($2)",
                room_id as &RoomId,
                &playlist_id_strs,
            )
            .execute(&mut **tx)
            .await
            {
                self.abort_playback_reset_fence_option(playback_fence.as_ref())
                    .await;
                return Err(error.into());
            }
            if let Err(error) = sqlx::query!(
                "WITH RECURSIVE target AS (SELECT id FROM playlists WHERE id = ANY($1) AND deleted_at IS NULL UNION ALL SELECT p.id FROM playlists p JOIN target t ON p.parent_id = t.id WHERE p.deleted_at IS NULL) UPDATE playlists SET deleted_at = COALESCE(deleted_at, CURRENT_TIMESTAMP), deletion_source = COALESCE(deletion_source, 'account'), deleted_owner_id = COALESCE(deleted_owner_id, $2), version = version + 1 WHERE id IN (SELECT id FROM target) AND deleted_at IS NULL",
                &playlist_id_strs,
                deleted_owner_id.as_i64(),
            )
            .execute(&mut **tx)
            .await
            {
                self.abort_playback_reset_fence_option(playback_fence.as_ref())
                    .await;
                return Err(error.into());
            }
        }

        Ok(UserDeletedRoomImpact {
            room_id: *room_id,
            deleted_media_ids,
            playback_reset: playback_state.is_some(),
            playback_state,
            playback_fence,
        })
    }
}
