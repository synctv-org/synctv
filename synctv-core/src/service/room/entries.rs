use std::{collections::HashSet, future::Future};

use crate::{
    models::{MediaId, PlaylistId, RoomId, RoomPermission, UserId},
    Error, Result,
};

use super::{
    apply_delete_entries_impact_in_tx, delete_entries_result_from_impact,
    ensure_actor_has_room_permission_now_tx, has_active_room_membership_in_tx,
    has_room_permission_in_tx, plan_clear_playlist_scope_in_tx, plan_delete_entries_in_room_in_tx,
    AuthorizedAdminActor, ClearPlaylistResult, DeleteEntriesPlan, DeleteEntriesRequest,
    DeleteEntriesResult, RealtimeOutboxDeleteEntriesEventFactory, RoomService, MAX_DELETE_TARGETS,
};

impl RoomService {
    pub async fn remove_media(
        &self,
        room_id: RoomId,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<()> {
        self.delete_entries(
            room_id,
            user_id,
            DeleteEntriesRequest {
                playlist_ids: Vec::new(),
                media_ids: vec![media_id],
                force: false,
            },
        )
        .await?;
        Ok(())
    }

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

    pub async fn delete_entries_with_precommit<T, F, Fut>(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: DeleteEntriesRequest,
        precommit: F,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.delete_entries_with_precommit_and_outbox(room_id, user_id, request, precommit, None)
            .await
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
        let playlist_ids = dedup_ids(request.playlist_ids);
        let media_ids = dedup_ids(request.media_ids);
        let force = request.force;
        let total_targets = playlist_ids.len() + media_ids.len();

        if total_targets == 0 {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        if total_targets > MAX_DELETE_TARGETS {
            return Err(Error::InvalidInput(format!(
                "Delete batch size exceeds maximum of {MAX_DELETE_TARGETS}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(&room_id, &playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        let mut impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;

        let affected_playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(&room_id, &impact.deleted_playlist_ids, &mut *tx)
            .await?;
        if affected_playlists.len() != impact.deleted_playlist_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a playlist that no longer exists".to_string(),
            ));
        }

        let affected_media = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &impact.deleted_media_ids, &mut *tx)
            .await?;
        if affected_media.len() != impact.deleted_media_ids.len() {
            return Err(Error::Internal(
                "Delete plan referenced a media item that no longer exists".to_string(),
            ));
        }

        let has_foreign_resources = affected_playlists
            .iter()
            .any(|playlist| playlist.creator_id.as_ref() != Some(&user_id))
            || affected_media
                .iter()
                .any(|media| media.creator_id.as_ref() != Some(&user_id));

        if !has_active_room_membership_in_tx(&mut tx, &room_id, &user_id).await? {
            return Err(Error::Authorization(
                synctv_common::messages::NOT_A_MEMBER_OF_THIS_ROOM.to_string(),
            ));
        }

        if has_foreign_resources
            && !has_room_permission_in_tx(
                &mut tx,
                &self.permission_service,
                &room_id,
                &user_id,
                RoomPermission::DELETE_MEDIA_RESOURCE_ANY,
            )
            .await?
        {
            return Err(Error::Authorization(
                synctv_common::messages::PERMISSION_DENIED.to_string(),
            ));
        }
        let plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: None,
        };
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        let should_notify_playlist_delete = !impact.deleted_playlist_ids.is_empty();
        if !impact.deleted_media_ids.is_empty() || should_notify_playlist_delete {
            let actor_username = match self.resolve_actor_username(&user_id).await {
                Ok(username) => username,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        user_id = %user_id,
                        "Skipped delete entries notifications because actor username lookup failed"
                    );
                    return Ok((delete_entries_result_from_impact(impact), precommit_result));
                }
            };
            for media_id in &impact.deleted_media_ids {
                let subscriber_count = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *media_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Media removed event had no local subscribers"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                let subscriber_count = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&user_id),
                    &actor_username,
                    *playlist_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Playlist deleted event had no local subscribers"
                    );
                }
            }
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

    /// Delete a mixed set of media and playlists as a global admin.
    pub async fn admin_delete_entries(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        let actor = self.load_authorized_admin_actor(&admin_user_id).await?;
        self.admin_delete_entries_as(room_id, &actor, request).await
    }

    pub async fn admin_delete_entries_as_with_precommit<T, F, Fut>(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
        precommit: F,
    ) -> Result<(DeleteEntriesResult, T)>
    where
        F: FnOnce(DeleteEntriesPlan) -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        self.admin_delete_entries_as_with_precommit_and_outbox(
            room_id, actor, request, precommit, None,
        )
        .await
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

        let playlist_ids = dedup_ids(request.playlist_ids);
        let media_ids = dedup_ids(request.media_ids);
        let force = request.force;
        let total_targets = playlist_ids.len() + media_ids.len();

        if total_targets == 0 {
            return Ok((
                DeleteEntriesResult::default(),
                precommit(DeleteEntriesPlan::default()).await?,
            ));
        }

        if total_targets > MAX_DELETE_TARGETS {
            return Err(Error::InvalidInput(format!(
                "Delete batch size exceeds maximum of {MAX_DELETE_TARGETS}"
            )));
        }

        let mut tx = self.pool.begin().await?;

        let playlists = self
            .playlist_repo
            .get_by_room_and_ids_with_executor(&room_id, &playlist_ids, &mut *tx)
            .await?;
        if playlists.len() != playlist_ids.len() {
            return Err(Error::NotFound(
                "One or more playlists not found".to_string(),
            ));
        }

        let media_items = self
            .media_repo
            .get_by_room_and_ids_with_executor(&room_id, &media_ids, &mut *tx)
            .await?;
        if media_items.len() != media_ids.len() {
            return Err(Error::NotFound(
                "One or more media items not found".to_string(),
            ));
        }

        let mut impact =
            plan_delete_entries_in_room_in_tx(&mut tx, &room_id, &playlist_ids, &media_ids, force)
                .await?;
        let plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: None,
        };
        let precommit_result = precommit(plan.clone()).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        if !impact.deleted_media_ids.is_empty() || !impact.deleted_playlist_ids.is_empty() {
            for media_id in &impact.deleted_media_ids {
                let subscriber_count = self.notification_service.notify_media_removed(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *media_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        media_id = %media_id,
                        "Media removed event had no local subscribers"
                    );
                }
            }
            for playlist_id in &impact.deleted_playlist_ids {
                let subscriber_count = self.notification_service.notify_playlist_deleted(
                    &room_id,
                    Some(&admin_user_id),
                    actor.username(),
                    *playlist_id,
                );
                if subscriber_count == 0 {
                    tracing::debug!(
                        room_id = %room_id,
                        playlist_id = %playlist_id,
                        "Playlist deleted event had no local subscribers"
                    );
                }
            }
        }

        tracing::info!(
            room_id = %room_id,
            admin_user_id = %admin_user_id,
            deleted_playlists = impact.deleted_playlist_ids.len(),
            deleted_media = impact.deleted_media_ids.len(),
            "Entries deleted by admin"
        );

        Ok((delete_entries_result_from_impact(impact), precommit_result))
    }

    pub async fn admin_delete_entries_as(
        &self,
        room_id: RoomId,
        actor: &AuthorizedAdminActor,
        request: DeleteEntriesRequest,
    ) -> Result<DeleteEntriesResult> {
        let (result, ()) = self
            .admin_delete_entries_as_with_precommit(room_id, actor, request, |_| async { Ok(()) })
            .await?;
        Ok(result)
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

    /// Clear media and child playlists in a playlist scope.
    ///
    /// The `CLEAR_MEDIA_RESOURCES` permission check is performed inside the
    /// transaction so revocations cannot race with the clear operation.
    ///
    /// `playlist_id = None` clears the room-root scope. `Some(id)` clears the
    /// given playlist's contents while keeping the playlist itself.
    pub async fn clear_playlist(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
    ) -> Result<ClearPlaylistResult> {
        self.clear_playlist_with_outbox(room_id, user_id, playlist_id, None)
            .await
    }

    pub async fn clear_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: Option<PlaylistId>,
        outbox_event_factory: Option<RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<ClearPlaylistResult> {
        let mut tx = self.pool.begin().await?;
        ensure_actor_has_room_permission_now_tx(
            &mut tx,
            &self.permission_service,
            &room_id,
            &user_id,
            RoomPermission::CLEAR_MEDIA_RESOURCES,
        )
        .await?;

        if let Some(playlist_id) = playlist_id {
            let exists = sqlx::query_scalar!(
                r#"SELECT EXISTS(
                    SELECT 1
                    FROM playlists
                    WHERE room_id = $1 AND id = $2
                ) AS "exists!""#,
                room_id.as_i64(),
                playlist_id.as_i64()
            )
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(Error::NotFound("Playlist not found".to_string()));
            }
        }

        let mut impact = plan_clear_playlist_scope_in_tx(&mut tx, &room_id, playlist_id).await?;
        apply_delete_entries_impact_in_tx(&mut tx, &room_id, &mut impact).await?;
        let committed_plan = DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        };
        let outbox_events = outbox_event_factory
            .as_ref()
            .map(|factory| factory(&committed_plan))
            .transpose()?
            .unwrap_or_default();
        if let Some(outbox) = &self.realtime_outbox {
            for event in &outbox_events {
                outbox.insert_with_executor(event, &mut *tx).await?;
            }
        }

        tx.commit().await?;

        if let Some(state) = impact.playback_state.clone() {
            self.broadcast_playback_reset_after_entry_deletion(state)
                .await;
        }
        self.cleanup_deleted_media_file_references(&impact.deleted_media_file_references)
            .await;

        let actor_username = match self.resolve_actor_username(&user_id).await {
            Ok(username) => username,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    room_id = %room_id,
                    user_id = %user_id,
                    "Skipped clear playlist notifications because actor username lookup failed"
                );
                return clear_playlist_result_from_impact(impact);
            }
        };
        for media_id in &impact.deleted_media_ids {
            let subscriber_count = self.notification_service.notify_media_removed(
                &room_id,
                Some(&user_id),
                &actor_username,
                *media_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    media_id = %media_id,
                    "Media removed event after clear_playlist had no local subscribers"
                );
            }
        }
        for playlist_id in &impact.deleted_playlist_ids {
            let subscriber_count = self.notification_service.notify_playlist_deleted(
                &room_id,
                Some(&user_id),
                &actor_username,
                *playlist_id,
            );
            if subscriber_count == 0 {
                tracing::debug!(
                    room_id = %room_id,
                    playlist_id = %playlist_id,
                    "Playlist deleted event after clear_playlist had no local subscribers"
                );
            }
        }

        clear_playlist_result_from_impact(impact)
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

fn clear_playlist_result_from_impact(
    impact: super::EntryDeletionImpact,
) -> Result<ClearPlaylistResult> {
    Ok(ClearPlaylistResult {
        deleted_count: deleted_count_to_i64(impact.deleted_media_ids.len(), "deleted media count")?,
        deleted_playlists: impact.deleted_playlist_ids.len(),
        deleted_playlist_ids: impact.deleted_playlist_ids,
        deleted_media_ids: impact.deleted_media_ids,
        playback_state: impact.playback_state,
    })
}

fn deleted_count_to_i64(value: usize, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Internal(format!("{field} exceeds i64::MAX")))
}
