use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPlaybackState, UserId},
    Result,
};

use super::{apply_delete_entries_impact_in_tx, plan_delete_entries_in_room_in_tx};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberResourceCleanupResult {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

impl MemberResourceCleanupResult {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deleted_playlist_ids.is_empty() && self.deleted_media_ids.is_empty()
    }
}

async fn collect_member_owned_root_playlist_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<Vec<PlaylistId>> {
    let playlist_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: PlaylistId"
           FROM playlists
           WHERE room_id = $1
             AND creator_id = $2
             AND deleted_at IS NULL
             AND (
                 parent_id IS NULL
                 OR parent_id NOT IN (
                     SELECT id
                     FROM playlists
                     WHERE room_id = $1
                       AND creator_id = $2
                       AND deleted_at IS NULL
                 )
             )
           ORDER BY id"#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(playlist_ids)
}

async fn collect_member_owned_root_media_ids_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<Vec<MediaId>> {
    let media_ids = sqlx::query_scalar!(
        r#"SELECT id AS "id: MediaId"
           FROM media
           WHERE room_id = $1
             AND creator_id = $2
             AND deleted_at IS NULL
             AND (
                 playlist_id IS NULL
                 OR playlist_id NOT IN (
                     SELECT id
                     FROM playlists
                     WHERE room_id = $1
                       AND creator_id = $2
                       AND deleted_at IS NULL
                 )
             )
           ORDER BY id"#,
        room_id.as_i64(),
        user_id.as_i64()
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(media_ids)
}

pub(super) async fn cleanup_member_resources_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    room_id: &RoomId,
    user_id: &UserId,
) -> Result<MemberResourceCleanupResult> {
    let playlist_ids = collect_member_owned_root_playlist_ids_in_tx(tx, room_id, user_id).await?;
    let media_ids = collect_member_owned_root_media_ids_in_tx(tx, room_id, user_id).await?;

    if playlist_ids.is_empty() && media_ids.is_empty() {
        return Ok(MemberResourceCleanupResult::default());
    }

    let mut impact =
        plan_delete_entries_in_room_in_tx(tx, room_id, &playlist_ids, &media_ids, true).await?;
    apply_delete_entries_impact_in_tx(tx, room_id, &mut impact).await?;

    Ok(MemberResourceCleanupResult {
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
        playback_reset: impact.playback_reset,
        playback_state: impact.playback_state,
    })
}
