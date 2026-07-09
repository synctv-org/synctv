use std::collections::HashSet;

use crate::{
    models::{Media, MediaId, Playlist, PlaylistId, RoomId, UserId},
    Error, Result,
};

use super::{DeleteEntriesPlan, DeleteEntriesRequest, EntryDeletionImpact, RoomService};

const MAX_DELETE_TARGETS: usize = 100;

pub(super) struct NormalizedDeleteEntriesRequest {
    pub playlist_ids: Vec<PlaylistId>,
    pub media_ids: Vec<MediaId>,
    pub force: bool,
}

impl NormalizedDeleteEntriesRequest {
    pub fn is_empty(&self) -> bool {
        self.playlist_ids.is_empty() && self.media_ids.is_empty()
    }
}

pub(super) struct DeleteEntryTargets {
    pub playlists: Vec<Playlist>,
    pub media_items: Vec<Media>,
}

impl DeleteEntryTargets {
    pub fn has_resources_not_owned_by(&self, user_id: &UserId) -> bool {
        self.playlists
            .iter()
            .any(|playlist| playlist.creator_id.as_ref() != Some(user_id))
            || self
                .media_items
                .iter()
                .any(|media| media.creator_id.as_ref() != Some(user_id))
    }
}

pub(super) fn normalize_delete_entries_request(
    request: DeleteEntriesRequest,
) -> Result<NormalizedDeleteEntriesRequest> {
    let playlist_ids = dedup_ids(request.playlist_ids);
    let media_ids = dedup_ids(request.media_ids);
    let total_targets = playlist_ids.len() + media_ids.len();

    if total_targets > MAX_DELETE_TARGETS {
        return Err(Error::InvalidInput(format!(
            "Delete batch size exceeds maximum of {MAX_DELETE_TARGETS}"
        )));
    }

    Ok(NormalizedDeleteEntriesRequest {
        playlist_ids,
        media_ids,
        force: request.force,
    })
}

pub(super) fn pending_delete_entries_plan(impact: &EntryDeletionImpact) -> DeleteEntriesPlan {
    DeleteEntriesPlan {
        deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
        deleted_media_ids: impact.deleted_media_ids.clone(),
        playback_reset: impact.playback_reset,
        playback_state: None,
    }
}

fn dedup_ids<T>(ids: Vec<T>) -> Vec<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(ids.len());
    for id in ids {
        if seen.insert(id.clone()) {
            deduped.push(id);
        }
    }
    deduped
}

impl RoomService {
    pub(super) async fn load_delete_entry_targets_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        playlist_ids: &[PlaylistId],
        media_ids: &[MediaId],
    ) -> Result<DeleteEntryTargets> {
        let playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(room_id, playlist_ids, &mut **tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(room_id, media_ids, &mut **tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        Ok(DeleteEntryTargets {
            playlists,
            media_items,
        })
    }

    pub(super) async fn load_planned_delete_entry_targets_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        room_id: &RoomId,
        impact: &EntryDeletionImpact,
    ) -> Result<DeleteEntryTargets> {
        let playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(room_id, &impact.deleted_playlist_ids, &mut **tx)
            .await?;
        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(room_id, &impact.deleted_media_ids, &mut **tx)
            .await?;

        if playlists.len() != impact.deleted_playlist_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a playlist that no longer exists".to_string(),
            ));
        }
        if media_items.len() != impact.deleted_media_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a media item that no longer exists".to_string(),
            ));
        }

        Ok(DeleteEntryTargets {
            playlists,
            media_items,
        })
    }
}
