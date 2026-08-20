use std::future::Future;

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPermission, RoomPlaybackState, UserId},
    Error, Result,
};

use super::{
    apply_delete_entries_impact_in_tx, delete_entries_result_from_impact,
    has_active_room_membership_in_tx, normalize_delete_entries_request,
    pending_delete_entries_plan, plan_delete_entries_in_room_in_tx, AuthorizedAdminActor,
    RealtimeOutboxDeleteEntriesEventFactory, RoomService,
};

#[derive(Debug, Clone, Default)]
pub struct DeleteEntriesRequest {
    pub playlist_ids: Vec<PlaylistId>,
    pub media_ids: Vec<MediaId>,
    pub force: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteEntriesResult {
    pub deleted_playlists: usize,
    pub deleted_media: usize,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeleteEntriesPlan {
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EntryDeletionImpact {
    pub playlist_nodes: Vec<(PlaylistId, i32)>,
    pub deleted_playlist_ids: Vec<PlaylistId>,
    pub deleted_media_ids: Vec<MediaId>,
    pub deleted_media_file_references: Vec<crate::models::FileReferenceTarget>,
    pub playback_reset: bool,
    pub playback_state: Option<RoomPlaybackState>,
}

impl RoomService {
    /// Delete a mixed set of playlists and media in one transaction.
    pub async fn delete_entries(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        self.delete_entries_with_outbox(room_id, user_id, request, None)
            .await
    }

    pub async fn delete_entries_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .delete_entries_with_precommit_and_outbox(
                room_id,
                user_id,
                request,
                |_| async { Ok(()) },
                outbox_event_factory,
            )
            .await?;
        Ok(result)
    }

    async fn delete_entries_with_precommit_and_outbox<T, F, Fut>(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        precommit: F,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let request = normalize_delete_entries_request(request)?;
        if request.is_empty() {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        let mut tx = self.pool.begin().await?;

        self.load_delete_entry_targets_tx(
            &mut tx,
            &room_id,
            &request.playlist_ids,
            &request.media_ids,
        )
        .await?;

        let mut impact = plan_delete_entries_in_room_in_tx(
            &mut tx,
            &room_id,
            &request.playlist_ids,
            &request.media_ids,
            request.force,
        )
        .await?;

        let affected_targets = self
            .load_planned_delete_entry_targets_tx(&mut tx, &room_id, &impact)
            .await?;

        if !has_active_room_membership_in_tx(&mut tx, &room_id, &user_id).await? {
            return Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        }

        if affected_targets.has_resources_not_owned_by(&user_id)
            && !self
                .has_room_permission_in_tx(
                    &mut tx,
                    &room_id,
                    &user_id,
                    RoomPermission::DELETE_MEDIA,
                )
                .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }
        let plan = pending_delete_entries_plan(&impact);
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        self.insert_delete_entries_outbox_events_tx(
            &mut tx,
            &impact,
            outbox_event_factory.as_ref(),
        )
        .await?;

        tx.commit().await?;

        self.finalize_entry_deletion_after_commit(&impact).await;
        if !self
            .notify_user_entry_deletion_after_commit(&room_id, &user_id, &impact)
            .await
        {
            return Ok((delete_entries_result_from_impact(impact), precommit_result));
        }

        tracing::info!(
            room_id = %room_id,
            user_id = %user_id,
            deleted_playlists = impact.deleted_playlist_ids.len(),
            deleted_media = impact.deleted_media_ids.len(),
            "Entries deleted"
        );

        Ok((delete_entries_result_from_impact(impact), precommit_result))
    }

    async fn admin_delete_entries_as_with_precommit_and_outbox<T, F, Fut>(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        precommit: F,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let admin_user_id = *actor.user_id();

        let request = normalize_delete_entries_request(request)?;
        if request.is_empty() {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        let mut tx = self.pool.begin().await?;

        self.load_delete_entry_targets_tx(
            &mut tx,
            &room_id,
            &request.playlist_ids,
            &request.media_ids,
        )
        .await?;

        let mut impact = plan_delete_entries_in_room_in_tx(
            &mut tx,
            &room_id,
            &request.playlist_ids,
            &request.media_ids,
            request.force,
        )
        .await?;
        let plan = pending_delete_entries_plan(&impact);
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        self.insert_delete_entries_outbox_events_tx(
            &mut tx,
            &impact,
            outbox_event_factory.as_ref(),
        )
        .await?;

        tx.commit().await?;

        self.finalize_entry_deletion_after_commit(&impact).await;
        self.notify_admin_entry_deletion_after_commit(
            &room_id,
            &admin_user_id,
            actor.username(),
            &impact,
        );

        tracing::info!(
            room_id = %room_id,
            admin_user_id = %admin_user_id,
            deleted_playlists = impact.deleted_playlist_ids.len(),
            deleted_media = impact.deleted_media_ids.len(),
            "Entries deleted by admin"
        );

        Ok((delete_entries_result_from_impact(impact), precommit_result))
    }

    pub async fn admin_delete_entries_as_with_outbox(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .admin_delete_entries_as_with_precommit_and_outbox(
                room_id,
                actor,
                request,
                |_| async { Ok(()) },
                outbox_event_factory,
            )
            .await?;
        Ok(result)
    }
}
