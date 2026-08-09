use std::collections::BTreeMap;

use crate::{
    models::{MediaId, PlaylistId, RoomId},
    Error, Result,
};

fn required_playlist_depth(depth: Option<i32>) -> Result<i32> {
    depth.ok_or_else(|| Error::Internal("playlist tree query did not return depth".to_string()))
}

async fn collect_all_room_playlist_nodes_in_tx(
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

async fn collect_room_root_media_ids_in_tx(
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

async fn collect_deleted_media_ids_in_tx(
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

async fn delete_playlist_ids_in_depth_order_in_tx(
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

pub(crate) async fn hard_delete_room_and_cleanup_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM rooms WHERE id = $1) AS "exists!""#,
        room_id.as_i64(),
    )
    .fetch_one(&mut **tx)
    .await?;
    if !exists {
        return Ok(false);
    }

    sqlx::query!(
        "UPDATE file_references fr SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP FROM rooms r WHERE r.id = $1 AND fr.id = r.cover_file_reference_id AND fr.released_at IS NULL",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;

    let playlist_nodes = collect_all_room_playlist_nodes_in_tx(tx, room_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let root_media_ids = collect_room_root_media_ids_in_tx(tx, room_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &root_media_ids)
            .await?;

    sqlx::query!(
        "DELETE FROM room_playback_state WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;

    if !deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = deleted_media_ids.iter().map(MediaId::as_i64).collect();
        sqlx::query!(
            "UPDATE file_references fr SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP FROM media m WHERE m.id = ANY($1) AND (fr.id = m.cover_file_reference_id OR fr.id = m.thumbnail_file_reference_id) AND fr.released_at IS NULL",
            &media_id_strs,
        )
        .execute(&mut **tx)
        .await?;
        sqlx::query!("DELETE FROM media WHERE id = ANY($1)", &media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    let playlist_id_strs: Vec<i64> = deleted_playlist_ids
        .iter()
        .map(PlaylistId::as_i64)
        .collect();
    if !playlist_id_strs.is_empty() {
        sqlx::query!(
            "UPDATE file_references fr SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP), updated_at = CURRENT_TIMESTAMP FROM playlists p WHERE p.id = ANY($1) AND fr.id = p.cover_file_reference_id AND fr.released_at IS NULL",
            &playlist_id_strs,
        )
        .execute(&mut **tx)
        .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &playlist_nodes).await?;

    sqlx::query!(
        "DELETE FROM room_members WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM room_settings WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    // Room resource events have an intentionally loose schema so they can
    // record aggregates that outlive individual rows. A hard-deleted room
    // has no replayable resource graph, so remove its durable events here.
    sqlx::query!(
        "DELETE FROM room_resource_events WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    // Chat attachments own a file reference whose row is independent from
    // the attachment row. Expire those references before the message FK
    // cascade removes the attachment rows so the storage cleanup worker can
    // release the backend object after the transaction commits.
    sqlx::query!(
        r#"
        UPDATE file_references fr
        SET expires_at = COALESCE(fr.expires_at, CURRENT_TIMESTAMP),
            updated_at = CURRENT_TIMESTAMP
        FROM chat_message_attachments a
        WHERE a.room_id = $1
          AND fr.storage_backend = a.storage_backend
          AND fr.object_key = a.object_key
          AND fr.reference_kind = 'chat_message_attachment'
          AND fr.reference_id = format(
              '%s:%s:%s:%s',
              a.room_id,
              a.message_id,
              round(extract(epoch FROM a.message_created_at) * 1000000)::BIGINT,
              a.id
          )
          AND fr.released_at IS NULL
        "#,
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM chat_messages WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!(
        "DELETE FROM room_join_requests WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?;
    sqlx::query!("DELETE FROM room_bans WHERE room_id = $1", room_id.as_i64(),)
        .execute(&mut **tx)
        .await?;

    let deleted = sqlx::query!("DELETE FROM rooms WHERE id = $1", room_id.as_i64())
        .execute(&mut **tx)
        .await?;

    Ok(deleted.rows_affected() > 0)
}
