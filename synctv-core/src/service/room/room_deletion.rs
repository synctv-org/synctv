use crate::{
    models::{MediaId, PlaylistId, RoomId, UserId},
    repository::room_member::RemovedRoomMember,
    Error, Result,
};

use super::{
    collect_all_room_playlist_nodes_in_tx, collect_deleted_media_ids_in_tx,
    collect_room_root_media_ids_in_tx, delete_playlist_ids_in_depth_order_in_tx,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoomCleanupImpact {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub members_deleted: u64,
    pub removed_members: Vec<RemovedRoomMember>,
    pub settings_deleted: u64,
    pub playback_rows_deleted: u64,
    pub chat_deleted: u64,
}

pub(crate) async fn soft_delete_room_and_cleanup_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
) -> Result<RoomCleanupImpact> {
    let now = crate::SystemClock.now();
    let deleted = sqlx::query!(
        r#"UPDATE rooms
         SET deleted_at = $2, updated_at = $2, version = version + 1
         WHERE id = $1 AND deleted_at IS NULL"#,
        room_id.as_i64(),
        now,
    )
    .execute(&mut **tx)
    .await?;

    if deleted.rows_affected() == 0 {
        return Err(Error::NotFound(
            "Room not found or already deleted".to_string(),
        ));
    }

    let playlist_nodes = collect_all_room_playlist_nodes_in_tx(tx, room_id).await?;
    let deleted_playlist_ids: Vec<PlaylistId> = playlist_nodes
        .iter()
        .map(|(playlist_id, _)| *playlist_id)
        .collect();
    let root_media_ids = collect_room_root_media_ids_in_tx(tx, room_id).await?;
    let deleted_media_ids =
        collect_deleted_media_ids_in_tx(tx, room_id, &deleted_playlist_ids, &root_media_ids)
            .await?;

    let playback_rows_deleted = sqlx::query!(
        "DELETE FROM room_playback_state WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    if !deleted_media_ids.is_empty() {
        let media_id_strs: Vec<i64> = deleted_media_ids.iter().map(MediaId::as_i64).collect();
        sqlx::query!("DELETE FROM media WHERE id = ANY($1)", &media_id_strs)
            .execute(&mut **tx)
            .await?;
    }

    delete_playlist_ids_in_depth_order_in_tx(tx, &playlist_nodes).await?;

    let mut removed_members: Vec<RemovedRoomMember> = sqlx::query!(
        r#"DELETE FROM room_members
         WHERE room_id = $1
         RETURNING room_id as "room_id: RoomId",
                   user_id as "user_id: UserId",
                   version"#,
        room_id as &RoomId,
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|row| RemovedRoomMember {
        room_id: row.room_id,
        user_id: row.user_id,
        version: row.version,
    })
    .collect();
    for member in &mut removed_members {
        member.version = sqlx::query_scalar!(
            "INSERT INTO room_member_versions (room_id, user_id, version, is_member, updated_at)
             VALUES ($1, $2, $3::BIGINT + 1, FALSE, CURRENT_TIMESTAMP)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET version = GREATEST(room_member_versions.version + 1, EXCLUDED.version),
                 is_member = FALSE,
                 updated_at = CURRENT_TIMESTAMP
             RETURNING version",
            &member.room_id as &RoomId,
            &member.user_id as &UserId,
            member.version,
        )
        .fetch_one(&mut **tx)
        .await?;
    }
    let members_deleted = removed_members.len() as u64;

    let settings_deleted = sqlx::query!(
        "DELETE FROM room_settings WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    let chat_deleted = sqlx::query!(
        "DELETE FROM chat_messages WHERE room_id = $1",
        room_id.as_i64(),
    )
    .execute(&mut **tx)
    .await?
    .rows_affected();

    Ok(RoomCleanupImpact {
        deleted_playlist_ids,
        deleted_media_ids,
        members_deleted,
        removed_members,
        settings_deleted,
        playback_rows_deleted,
        chat_deleted,
    })
}
