use std::sync::Arc;

use crate::{
    models::{MediaId, PlaylistId, RoomId, UserId},
    provider::{DirectoryItem, DynamicListQuery, ProviderContext},
    service::media::MediaService,
    Error, Result,
};

impl MediaService {
    async fn get_dynamic_playlist_provider(
        &self,
        playlist: &crate::models::Playlist,
    ) -> Result<(String, Arc<dyn crate::provider::MediaProvider>)> {
        let provider_name = playlist
            .source_provider
            .clone()
            .ok_or_else(|| Error::InvalidInput("Dynamic playlist missing provider".to_string()))?;

        let bound_instance = playlist.provider_instance_name.as_deref().and_then(|name| {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        });

        let provider = self
            .providers_manager
            .resolve_provider(&provider_name, bound_instance)
            .await?;

        Ok((provider_name, provider))
    }

    pub async fn admin_list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
        query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>> {
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let ctx = self.build_provider_context(
            &admin_user_id,
            &room_id,
            playlist.creator_id.as_ref().or(Some(&admin_user_id)),
            playlist.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .list_playlist(&ctx, &playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn admin_get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<crate::provider::DynamicBrowsePathSegment>> {
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let ctx = self.build_provider_context(
            &admin_user_id,
            &room_id,
            playlist.creator_id.as_ref().or(Some(&admin_user_id)),
            playlist.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .browse_path(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
        query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::VIEW_MEDIA_RESOURCES,
            )
            .await?;

        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let ctx = self.build_provider_context(
            &user_id,
            &room_id,
            playlist.creator_id.as_ref().or(Some(&user_id)),
            playlist.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .list_playlist(&ctx, &playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<crate::provider::DynamicBrowsePathSegment>> {
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let ctx = self.build_provider_context(
            &user_id,
            &room_id,
            playlist.creator_id.as_ref().or(Some(&user_id)),
            playlist.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .browse_path(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn resolve_dynamic_playlist_item(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: &[u8],
    ) -> Result<Option<crate::provider::NextPlayItem>> {
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(&room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;

        let ctx = self.build_provider_context(
            &user_id,
            &room_id,
            playlist.creator_id.as_ref().or(Some(&user_id)),
            playlist.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .resolve_item(&ctx, &playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn next_dynamic_playlist_item(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        target: &[u8],
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<crate::provider::NextPlayItem>> {
        let playlist = self
            .playlist_repo
            .get_by_room_and_id(room_id, playlist_id)
            .await?
            .ok_or_else(|| Error::NotFound("Playlist not found".to_string()))?;

        if !playlist.is_dynamic() {
            return Err(Error::InvalidInput("Playlist is not dynamic".to_string()));
        }

        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;

        let dynamic_folder = provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {provider_name} does not support dynamic folders"
            ))
        })?;
        let current_dynamic_media = crate::models::Media {
            id: MediaId::new(),
            playlist_id: Some(playlist.id),
            room_id: *room_id,
            creator_id: playlist.creator_id,
            name: format!("dynamic:{playlist_id}"),
            description: String::new(),
            position: 0.0,
            source_provider: provider_name.clone(),
            source_config: serde_json::Value::Null,
            provider_instance_name: playlist.provider_instance_name.clone(),
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let mut ctx = ProviderContext::new("synctv").with_room_id(*room_id);
        if let Some(creator_id) = playlist.creator_id.as_ref() {
            ctx = ctx.with_credential_owner_id(*creator_id);
        }
        if let Some(provider_instance_name) =
            current_dynamic_media.provider_instance_name.as_deref()
        {
            ctx = ctx.with_provider_instance_name(provider_instance_name);
        }
        if let Some(ref repo) = self.credential_repo {
            ctx = ctx.with_credential_repo(repo);
        }
        if let Some(ref enc) = self.credential_encryption {
            ctx = ctx.with_credential_encryption(enc);
        }

        dynamic_folder
            .next(&ctx, &playlist, &current_dynamic_media, target, play_mode)
            .await
            .map_err(Error::from)
    }
}
