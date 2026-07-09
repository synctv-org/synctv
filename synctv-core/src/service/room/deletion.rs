use std::collections::BTreeMap;

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackState},
    Error, Result,
};

use super::{DeleteEntriesResult, EntryDeletionImpact};

struct MediaFileReferenceRow {
    id: MediaId,
    storage_backend: String,
    object_key: String,
    reference_kind: String,
}

fn required_playlist_depth(depth: Option<i32>) -> Result<i32> {
    depth.ok_or_else(|| Error::Internal("playlist tree query did not return depth".to_string()))
}

async fn collect_target_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    root_playlist_ids: &[PlaylistId],
) -> Result<Vec<(PlaylistId, i32)>> {
    if root_playlist_ids.is_empty() {
        return Ok(Vec::new());
    }

    let playlist_ids: Vec<i64> = root_playlist_ids.iter().map(PlaylistId::as_i64).collect();

    let rows = sqlx::query!(
        r#"WITH RECURSIVE target_playlists AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND id = ANY($2)
            UNION ALL
            SELECT p.id, tp.depth + 1
            FROM playlists p
            JOIN target_playlists tp ON p.parent_id = tp.id
            WHERE p.room_id = $1
        )
        SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
        FROM target_playlists
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64(),
        &playlist_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.id, required_playlist_depth(row.depth)?));
    }
    Ok(result)
}

pub(super) async fn collect_all_room_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<(PlaylistId, i32)>> {
    let rows = sqlx::query!(
        r#"WITH RECURSIVE playlist_tree AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND parent_id IS NULL
            UNION ALL
            SELECT p.id, pt.depth + 1
            FROM playlists p
            JOIN playlist_tree pt ON p.parent_id = pt.id
            WHERE p.room_id = $1
        )
        SELECT id AS "id!: PlaylistId", MAX(depth) AS depth
        FROM playlist_tree
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.id, required_playlist_depth(row.depth)?));
    }
    Ok(result)
}

pub(super) async fn collect_room_root_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
         FROM media
         WHERE room_id = $1
           AND playlist_id IS NULL
         ORDER BY id"#,
        room_id.as_i64(),
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

pub(super) async fn collect_deleted_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_ids: &[PlaylistId],
    explicit_media_ids: &[MediaId],
) -> Result<Vec<MediaId>> {
    if playlist_ids.is_empty() && explicit_media_ids.is_empty() {
        return Ok(Vec::new());
    }

    let playlist_id_strs: Vec<i64> = playlist_ids.iter().map(PlaylistId::as_i64).collect();
    let explicit_media_id_strs: Vec<i64> = explicit_media_ids.iter().map(MediaId::as_i64).collect();

    let media_ids = sqlx::query_scalar!(
        r#"WITH RECURSIVE target_playlists AS (
            SELECT id
            FROM playlists
            WHERE id = ANY($1)
            UNION ALL
            SELECT p.id
            FROM playlists p
            JOIN target_playlists tp ON p.parent_id = tp.id
        )
        SELECT DISTINCT m.id AS "id: MediaId"
        FROM media m
        WHERE m.room_id = $2
          AND (
              m.id = ANY($3)
              OR m.playlist_id IN (SELECT id FROM target_playlists)
          )
        ORDER BY m.id"#,
        &playlist_id_strs,
        room_id.as_i64(),
        &explicit_media_id_strs
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

async fn collect_media_file_references_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    media_ids: &[MediaId],
) -> Result<Vec<crate::models::FileReferenceTarget>> {
    if media_ids.is_empty() {
        return Ok(Vec::new());
    }

    let media_id_strs: Vec<i64> = media_ids.iter().map(MediaId::as_i64).collect();
    let rows = sqlx::query_as!(
        MediaFileReferenceRow,
        r#"
        SELECT m.id AS "id!: MediaId",
               fr.storage_backend AS "storage_backend!",
               fr.object_key AS "object_key!",
               'media_cover' AS "reference_kind!"
          FROM media m
          JOIN file_references fr
            ON fr.id = m.cover_file_reference_id
           AND fr.released_at IS NULL
         WHERE m.room_id = $1
           AND m.id = ANY($2)
        UNION ALL
        SELECT m.id AS "id!: MediaId",
               fr.storage_backend AS "storage_backend!",
               fr.object_key AS "object_key!",
               'media_thumbnail' AS "reference_kind!"
          FROM media m
          JOIN file_references fr
            ON fr.id = m.thumbnail_file_reference_id
           AND fr.released_at IS NULL
         WHERE m.room_id = $1
           AND m.id = ANY($2)
        "#,
        room_id.as_i64(),
        &media_id_strs
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| crate::models::FileReferenceTarget {
            storage_backend: row.storage_backend,
            object_key: row.object_key,
            reference_kind: row.reference_kind,
            reference_id: row.id.to_string(),
        })
        .collect())
}

async fn plan_playback_reset_for_deleted_entries_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    deleted_playlist_ids: &[PlaylistId],
    deleted_media_ids: &[MediaId],
    force: bool,
) -> Result<bool> {
    let playback_row = sqlx::query!(
        r#"SELECT playing_media_id AS "playing_media_id?: MediaId",
                  playing_playlist_id AS "playing_playlist_id?: PlaylistId"
         FROM room_playback_state
         WHERE room_id = $1
         FOR UPDATE"#,
        room_id.as_i64(),
    )
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = playback_row else {
        return Ok(false);
    };

    let deletes_playing_media = row.playing_media_id.as_ref().is_some_and(|current_id| {
        deleted_media_ids
            .iter()
            .any(|media_id| media_id == current_id)
    });

    let deletes_playing_playlist = row.playing_playlist_id.as_ref().is_some_and(|current_id| {
        deleted_playlist_ids
            .iter()
            .any(|playlist_id| playlist_id == current_id)
    });

    if !(deletes_playing_media || deletes_playing_playlist) {
        return Ok(false);
    }

    if !force {
        return Err(Error::InvalidInput(
            "Cannot delete entries that include the currently playing media".to_string(),
        ));
    }

    Ok(true)
}

pub(super) async fn delete_playlist_ids_in_depth_order_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    playlist_nodes: &[(PlaylistId, i32)],
) -> Result<()> {
    if playlist_nodes.is_empty() {
        return Ok(());
    }

    let mut ids_by_depth = BTreeMap::<i32, Vec<i64>>::new();
    for (playlist_id, depth) in playlist_nodes {
        ids_by_depth
            .entry(*depth)
            .or_default()
            .push(playlist_id.as_i64());
    }

    for (_depth, ids) in ids_by_depth.into_iter().rev() {
        sqlx::query!("DELETE FROM playlists WHERE id = ANY($1)", &ids)
            .execute(&mut **tx)
            .await?;
    }

    Ok(())
}

pub(super) fn delete_entries_result_from_impact(
    impact: EntryDeletionImpact,
) -> DeleteEntriesResult {
    DeleteEntriesResult {
        deleted_playlists: impact.deleted_playlist_ids.len(),
        deleted_media: impact.deleted_media_ids.len(),
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
        playback_state: impact.playback_state,
    }
}

pub(super) async fn apply_delete_entries_impact_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    impact: &mut EntryDeletionImpact,
) -> Result<()> {
    if impact.playback_reset {
        let state = sqlx::query_as!(
            RoomPlaybackState,
            r#"WITH current_state AS (
                SELECT room_id, current_progress_id
                FROM room_playback_state
                WHERE room_id = $1
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
                    version = version + 1,
                    updated_at = NOW()
                FROM current_state
                WHERE state.room_id = current_state.room_id
                RETURNING state.room_id,
                          state.playing_media_id,
                          state.playing_playlist_id,
                          state.target,
                          state.current_progress_id,
                          state.speed,
                          state.is_playing,
                          state.updated_at,
                          state.version
            )
            SELECT room_id AS "room_id!: RoomId",
                   playing_media_id AS "playing_media_id?: MediaId",
                   playing_playlist_id AS "playing_playlist_id?: PlaylistId",
                   target AS "target?: crate::models::ProviderTarget",
                   current_progress_id,
                   0.0::DOUBLE PRECISION AS "position!",
                   speed AS "speed!",
                   is_playing AS "is_playing!",
                   updated_at AS "updated_at!",
                   version AS "version!"
            FROM updated"#,
            room_id.as_i64(),
        )
        .fetch_one(&mut **tx)
        .await?;
        impact.playback_state = Some(state);
    }

    if !impact.deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = impact
            .deleted_media_ids
            .iter()
            .map(MediaId::as_i64)
            .collect();
        sqlx::query!(
            "DELETE FROM room_playback_progress WHERE room_id = $1 AND media_id = ANY($2)",
            room_id.as_i64(),
            &media_id_strs,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!("DELETE FROM media WHERE id = ANY($1)", &media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    if !impact.deleted_playlist_ids.is_empty() {
        let playlist_id_strs: Vec<i64> = impact
            .deleted_playlist_ids
            .iter()
            .map(PlaylistId::as_i64)
            .collect();
        sqlx::query!(
            "DELETE FROM room_playback_progress WHERE room_id = $1 AND playlist_id = ANY($2)",
            room_id.as_i64(),
            &playlist_id_strs,
        )
        .execute(&mut **tx)
        .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &impact.playlist_nodes).await?;

    Ok(())
}

pub(super) async fn plan_delete_entries_in_room_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    root_playlist_ids: &[PlaylistId],
    explicit_media_ids: &[MediaId],
    force: bool,
) -> Result<EntryDeletionImpact> {
    let playlist_nodes =
        collect_target_playlist_nodes_in_tx(tx, room_id, root_playlist_ids).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, explicit_media_ids)
            .await?;
    let deleted_media_file_references =
        collect_media_file_references_in_tx(tx, room_id, &deleted_media_ids).await?;
    let playback_reset = plan_playback_reset_for_deleted_entries_in_tx(
        tx,
        room_id,
        &deleted_playlist_ids,
        &deleted_media_ids,
        force,
    )
    .await?;

    Ok(EntryDeletionImpact {
        playlist_nodes,
        deleted_playlist_ids,
        deleted_media_ids,
        deleted_media_file_references,
        playback_reset,
        playback_state: None,
    })
}

async fn collect_child_playlist_nodes_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    parent_playlist_id: Option<PlaylistId>,
) -> Result<Vec<(PlaylistId, i32)>> {
    let rows = sqlx::query!(
        r#"WITH RECURSIVE child_playlists AS (
            SELECT id, 0 AS depth
            FROM playlists
            WHERE room_id = $1
              AND (
                  ($2::BIGINT IS NULL AND parent_id IS NULL)
                  OR parent_id = $2
              )
            UNION ALL
            SELECT p.id, cp.depth + 1
            FROM playlists p
            JOIN child_playlists cp ON p.parent_id = cp.id
            WHERE p.room_id = $1
        )
        SELECT id AS "playlist_id!: PlaylistId", COALESCE(MAX(depth), 0) AS "depth!: i32"
        FROM child_playlists
        GROUP BY id
        ORDER BY MAX(depth) DESC, id"#,
        room_id.as_i64(),
        parent_playlist_id.map(|playlist_id| playlist_id.as_i64())
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        result.push((row.playlist_id, row.depth));
    }
    Ok(result)
}

async fn collect_direct_scope_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_id: Option<PlaylistId>,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
         FROM media
         WHERE room_id = $1
           AND (
               ($2::BIGINT IS NULL AND playlist_id IS NULL)
               OR playlist_id = $2
         )
         ORDER BY id"#,
        room_id.as_i64(),
        playlist_id.map(|playlist_id| playlist_id.as_i64())
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

pub(super) async fn plan_clear_playlist_scope_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    playlist_id: Option<PlaylistId>,
) -> Result<EntryDeletionImpact> {
    let playlist_nodes = collect_child_playlist_nodes_in_tx(tx, room_id, playlist_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let direct_media_ids = collect_direct_scope_media_ids_in_tx(tx, room_id, playlist_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &direct_media_ids)
            .await?;
    let deleted_media_file_references =
        collect_media_file_references_in_tx(tx, room_id, &deleted_media_ids).await?;
    let playback_reset = plan_playback_reset_for_deleted_entries_in_tx(
        tx,
        room_id,
        &deleted_playlist_ids,
        &deleted_media_ids,
        true,
    )
    .await?;

    Ok(EntryDeletionImpact {
        playlist_nodes,
        deleted_playlist_ids,
        deleted_media_ids,
        deleted_media_file_references,
        playback_reset,
        playback_state: None,
    })
}
