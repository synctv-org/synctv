use crate::{
    models::{
        Playlist, PlaylistBrowseAccessMode, PlaylistId, PlaylistSourceConfig, RoomId, UserId,
    },
    service::{optimistic_retry, provider_binding::provider_instance_name_for_source_update},
    Error, Result,
};

use super::{
    ensure_playlist_creator_can_edit, PlaylistService, RealtimeOutboxPlaylistEventFactory,
};

/// Request to set playlist properties
#[derive(Debug, Clone)]
pub struct SetPlaylistRequest {
    pub playlist_id: PlaylistId,
    pub name: Option<String>,
    pub description: Option<String>,
    pub browse_access_mode: Option<PlaylistBrowseAccessMode>,
    /// Complete replacement config for an existing dynamic playlist.
    pub source_config: Option<PlaylistSourceConfig>,
    /// Replacement provider instance, optionally updated without changing the config.
    pub provider_instance_name: Option<String>,
}

impl PlaylistService {
    pub async fn set_playlist_with_outbox(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.set_playlist_internal(room_id, user_id, request, false, outbox_event_factory)
            .await
    }

    pub async fn admin_set_playlist_with_outbox(
        &self,
        room_id: RoomId,
        actor_user_id: UserId,
        request: SetPlaylistRequest,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        self.set_playlist_internal(room_id, actor_user_id, request, true, outbox_event_factory)
            .await
    }

    async fn set_playlist_internal(
        &self,
        room_id: RoomId,
        user_id: UserId,
        request: SetPlaylistRequest,
        bypass_room_permissions: bool,
        outbox_event_factory: Option<RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<Playlist> {
        let playlist_id = request.playlist_id;
        let updated_playlist = optimistic_retry::retry_with_optimistic_lock(
            optimistic_retry::DEFAULT_MAX_RETRIES,
            optimistic_retry::DEFAULT_BACKOFF_BASE_MS,
            "Playlist update failed after maximum retry attempts",
            || async {
                // Get existing playlist (re-fetch on each retry to get latest version)
                let mut playlist = self
                    .playlist_repo
                    .get_by_room_and_id(&room_id, &request.playlist_id)
                    .await?
                    .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

                if !bypass_room_permissions {
                    ensure_playlist_creator_can_edit(&playlist, &user_id)?;
                    self.permission_service
                        .check_permission_no_cache(
                            &room_id,
                            &user_id,
                            crate::models::RoomPermission::MANAGE_OWN_MEDIA,
                        )
                        .await?;
                }

                // Store original version for optimistic locking
                let expected_version = playlist.version;

                // Update fields
                if let Some(ref name) = request.name {
                    if name.chars().count() > crate::validation::PLAYLIST_NAME_MAX {
                        return Err(Error::InvalidInput(format!(
                            "Playlist name cannot exceed {} characters",
                            crate::validation::PLAYLIST_NAME_MAX
                        )));
                    }
                    playlist.name = name.clone();
                }
                if let Some(ref description) = request.description {
                    if description.chars().count() > crate::validation::PLAYLIST_DESCRIPTION_MAX {
                        return Err(Error::InvalidInput(format!(
                            "Playlist description cannot exceed {} characters",
                            crate::validation::PLAYLIST_DESCRIPTION_MAX
                        )));
                    }
                    playlist.description = description.clone();
                }
                if let Some(browse_access_mode) = request.browse_access_mode {
                    playlist.browse_access_mode = browse_access_mode;
                }
                if request.source_config.is_some() || request.provider_instance_name.is_some() {
                    if !playlist.is_dynamic() {
                        return Err(Error::InvalidInput(
                            "source updates require a dynamic playlist".to_string(),
                        ));
                    }

                    let source_config = request
                        .source_config
                        .clone()
                        .or_else(|| playlist.source_config.clone())
                        .ok_or_else(|| {
                            Error::Internal(
                                "Dynamic playlist is missing its source_config".to_string(),
                            )
                        })?;
                    let source_provider = source_config.provider();
                    let provider_instance_name = provider_instance_name_for_source_update(
                        playlist.source_provider,
                        source_provider,
                        request.provider_instance_name.clone(),
                        playlist.provider_instance_name.clone(),
                    );
                    let (source_provider, source_config, provider_instance_name) = self
                        .validate_dynamic_playlist_source(
                            &room_id,
                            &user_id,
                            source_provider,
                            source_config,
                            provider_instance_name,
                        )
                        .await?;
                    playlist.source_provider = Some(source_provider);
                    playlist.source_config = Some(source_config);
                    playlist.provider_instance_name = provider_instance_name;
                }
                // Save with optimistic locking
                let mut tx = self.playlist_repo.pool().begin().await?;
                match self
                    .playlist_repo
                    .update_with_version_with_executor(&playlist, expected_version, &mut *tx)
                    .await
                {
                    Ok(updated_playlist) => {
                        self.insert_playlist_outbox_tx(
                            &mut tx,
                            &updated_playlist,
                            outbox_event_factory.as_ref(),
                        )
                        .await?;
                        tx.commit().await?;
                        Ok(updated_playlist)
                    }
                    Err(Error::OptimisticLockConflict) => {
                        tx.rollback().await?;
                        Err(Error::OptimisticLockConflict)
                    }
                    Err(e) => Err(e),
                }
            },
        )
        .await?;

        tracing::info!(
            room_id = %room_id,
            playlist_id = %playlist_id,
            "Playlist updated"
        );
        Ok(updated_playlist)
    }
}
