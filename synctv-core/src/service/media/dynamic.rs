use std::sync::Arc;

use crate::{
    models::{MediaId, Playlist, PlaylistId, RoomId, UserId},
    provider::{
        DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, MediaProvider,
        NextPlayItem, ProviderContext,
    },
    service::media::MediaService,
    Error, Result,
};

pub(super) struct PreparedDynamicPlaylist {
    pub(super) playlist: Playlist,
    pub(super) provider_name: String,
    pub(super) provider: Arc<dyn MediaProvider>,
}

impl PreparedDynamicPlaylist {
    pub(super) fn dynamic_folder(&self) -> Result<&dyn DynamicFolder> {
        self.provider.as_dynamic_folder().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {} does not support dynamic folders",
                self.provider_name
            ))
        })
    }
}

impl MediaService {
    pub(super) async fn prepare_dynamic_playlist(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
    ) -> Result<PreparedDynamicPlaylist> {
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

        Ok(PreparedDynamicPlaylist {
            playlist,
            provider_name,
            provider,
        })
    }

    pub(super) async fn get_dynamic_playlist_provider(
        &self,
        playlist: &Playlist,
    ) -> Result<(String, Arc<dyn MediaProvider>)> {
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

    pub(super) fn dynamic_playlist_context<'a>(
        &'a self,
        prepared: &'a PreparedDynamicPlaylist,
        user_id: Option<&'a UserId>,
        fallback_credential_owner_id: Option<&'a UserId>,
        public_credential_owner_id: Option<&'a str>,
    ) -> ProviderContext<'a> {
        let credential_owner_id = prepared
            .playlist
            .creator_id
            .as_ref()
            .or(fallback_credential_owner_id);
        self.build_provider_context(
            prepared.provider_name.as_str(),
            user_id,
            &prepared.playlist.room_id,
            credential_owner_id,
            public_credential_owner_id,
            prepared.playlist.provider_instance_name.as_deref(),
        )
    }

    pub async fn admin_list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
        query: DynamicListQuery,
        public_credential_owner_id: Option<&str>,
    ) -> Result<Vec<DirectoryItem>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(
            &prepared,
            Some(&admin_user_id),
            Some(&admin_user_id),
            public_credential_owner_id,
        );

        prepared
            .dynamic_folder()?
            .list_playlist(&ctx, &prepared.playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn admin_get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(
            &prepared,
            Some(&admin_user_id),
            Some(&admin_user_id),
            None,
        );

        prepared
            .dynamic_folder()?
            .browse_path(&ctx, &prepared.playlist, target)
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
        public_credential_owner_id: Option<&str>,
    ) -> Result<Vec<DirectoryItem>> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::VIEW_MEDIA_RESOURCES,
            )
            .await?;

        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(
            &prepared,
            Some(&user_id),
            Some(&user_id),
            public_credential_owner_id,
        );

        prepared
            .dynamic_folder()?
            .list_playlist(&ctx, &prepared.playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, Some(&user_id), Some(&user_id), None);

        prepared
            .dynamic_folder()?
            .browse_path(&ctx, &prepared.playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn resolve_dynamic_playlist_item(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, Some(&user_id), Some(&user_id), None);

        prepared
            .dynamic_folder()?
            .resolve_item(&ctx, &prepared.playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn next_dynamic_playlist_item(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        target: &[u8],
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>> {
        let prepared = self.prepare_dynamic_playlist(room_id, playlist_id).await?;
        let current_dynamic_media = crate::models::Media {
            id: MediaId::new(),
            playlist_id: Some(prepared.playlist.id),
            room_id: *room_id,
            creator_id: prepared.playlist.creator_id,
            name: format!("dynamic:{playlist_id}"),
            description: String::new(),
            position: 0.0,
            source_provider: prepared.provider_name.clone(),
            source_config: serde_json::Value::Null,
            provider_instance_name: prepared.playlist.provider_instance_name.clone(),
            cover_file_reference_id: None,
            added_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            version: 0,
        };

        let ctx = self.dynamic_playlist_context(&prepared, None, None, None);

        prepared
            .dynamic_folder()?
            .next(
                &ctx,
                &prepared.playlist,
                &current_dynamic_media,
                target,
                play_mode,
            )
            .await
            .map_err(Error::from)
    }
}

fn dynamic_playlist_source_provider(playlist: &Playlist) -> Result<&str> {
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
