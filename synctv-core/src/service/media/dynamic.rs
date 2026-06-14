use std::sync::Arc;

use crate::{
    models::{MediaId, PlaylistId, RoomId, UserId},
    provider::{DirectoryItem, DynamicListQuery},
    service::media::MediaService,
    Error, Result,
};

impl MediaService {
    pub(super) async fn get_dynamic_playlist_provider(
        &self,
        playlist: &crate::models::Playlist,
    ) -> Result<(String, Arc<dyn crate::provider::MediaProvider>)> {
        let provider_name = dynamic_playlist_source_provider(playlist)?.to_string();

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
            Some(&admin_user_id),
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
            Some(&admin_user_id),
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
            Some(&user_id),
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
            Some(&user_id),
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
            Some(&user_id),
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

        let ctx = self.build_provider_context(
            None,
            room_id,
            playlist.creator_id.as_ref(),
            current_dynamic_media.provider_instance_name.as_deref(),
        );

        dynamic_folder
            .next(&ctx, &playlist, &current_dynamic_media, target, play_mode)
            .await
            .map_err(Error::from)
    }
}

fn dynamic_playlist_source_provider(playlist: &crate::models::Playlist) -> Result<&str> {
    let provider = playlist
        .source_provider
        .as_deref()
        .ok_or_else(|| {
            Error::Internal(format!("Dynamic playlist {} missing provider", playlist.id))
        })?
        .trim();
    if provider.is_empty() {
        return Err(Error::Internal(format!(
            "Dynamic playlist {} has empty provider",
            playlist.id
        )));
    }
    if playlist.source_config.is_none() {
        return Err(Error::Internal(format!(
            "Dynamic playlist {} missing source_config",
            playlist.id
        )));
    }

    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Playlist;
    use chrono::Utc;

    fn dynamic_playlist(
        source_provider: Option<String>,
        source_config: Option<serde_json::Value>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(10),
            room_id: RoomId::expect_positive(20),
            creator_id: Some(UserId::expect_positive(30)),
            name: "Dynamic".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider,
            source_config,
            provider_instance_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version: 0,
        }
    }

    #[test]
    fn dynamic_playlist_source_provider_trims_valid_provider() {
        let playlist = dynamic_playlist(Some(" alist ".to_string()), Some(serde_json::json!({})));

        assert_eq!(
            dynamic_playlist_source_provider(&playlist)
                .expect("dynamic playlist provider should parse"),
            "alist"
        );
    }

    #[test]
    fn dynamic_playlist_source_provider_rejects_missing_provider() {
        let playlist = dynamic_playlist(None, Some(serde_json::json!({})));

        assert!(matches!(
            dynamic_playlist_source_provider(&playlist),
            Err(Error::Internal(message))
                if message.contains("Dynamic playlist")
                    && message.contains("provider")
        ));
    }

    #[test]
    fn dynamic_playlist_source_provider_rejects_empty_provider() {
        let playlist = dynamic_playlist(Some("   ".to_string()), Some(serde_json::json!({})));

        assert!(matches!(
            dynamic_playlist_source_provider(&playlist),
            Err(Error::Internal(message))
                if message.contains("Dynamic playlist")
                    && message.contains("provider")
        ));
    }

    #[test]
    fn dynamic_playlist_source_provider_rejects_missing_source_config() {
        let playlist = dynamic_playlist(Some("alist".to_string()), None);

        assert!(matches!(
            dynamic_playlist_source_provider(&playlist),
            Err(Error::Internal(message))
                if message.contains("Dynamic playlist")
                    && message.contains("source_config")
        ));
    }
}
