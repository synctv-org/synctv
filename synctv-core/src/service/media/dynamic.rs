use std::sync::Arc;

use crate::{
    models::{Playlist, PlaylistId, PlaylistSourceConfig, RoomId, SourceProvider, UserId},
    provider::{
        DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPlaylistProvider,
        MediaProvider, NextPlayItem, ProviderActor, ProviderContext,
    },
    Error, Result,
};

use super::MediaService;

pub(super) struct PreparedDynamicPlaylist {
    pub(super) playlist: Playlist,
    pub(super) provider_name: String,
    pub(super) provider: Arc<dyn MediaProvider>,
}

pub struct DynamicPlaylistPreviewRequest {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub source_provider: SourceProvider,
    pub source_config: PlaylistSourceConfig,
    pub provider_instance_name: Option<String>,
    pub target: Option<crate::models::ProviderTarget>,
    pub query: DynamicListQuery,
}

impl PreparedDynamicPlaylist {
    pub(super) fn dynamic_playlist_provider(&self) -> Result<&dyn DynamicPlaylistProvider> {
        self.provider.as_dynamic_playlist_provider().ok_or_else(|| {
            Error::InvalidInput(format!(
                "Provider {} does not support dynamic playlists",
                self.provider_name
            ))
        })
    }
}

impl MediaService {
    pub async fn playlist_provider_metadata(
        &self,
        actor: ProviderActor,
        playlist: &Playlist,
    ) -> Result<Option<crate::provider::ProviderResourceMetadata>> {
        let (provider_name, provider) = self.get_dynamic_playlist_provider(playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let prepared = PreparedDynamicPlaylist {
            playlist: playlist.clone(),
            provider_name,
            provider,
        };
        let ctx = self.dynamic_playlist_context(&prepared, actor);
        let source_config = prepared
            .playlist
            .source_config
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("Missing source_config".to_string()))?;
        let dynamic_provider = prepared.dynamic_playlist_provider()?;
        let metadata = match tokio::time::timeout(
            super::PROVIDER_METADATA_TIMEOUT,
            dynamic_provider.playlist_metadata(&ctx, source_config),
        )
        .await
        {
            Ok(Ok(metadata)) => metadata,
            Ok(Err(error)) => {
                tracing::debug!(
                    playlist_id = %playlist.id,
                    error = %error,
                    "provider playlist metadata unavailable"
                );
                None
            }
            Err(_) => {
                tracing::debug!(
                    playlist_id = %playlist.id,
                    timeout_ms = super::PROVIDER_METADATA_TIMEOUT.as_millis(),
                    "provider playlist metadata timed out"
                );
                None
            }
        };
        Ok(metadata)
    }

    pub async fn preview_dynamic_playlist_items(
        &self,
        request: DynamicPlaylistPreviewRequest,
    ) -> Result<DynamicListResult> {
        let DynamicPlaylistPreviewRequest {
            room_id,
            user_id,
            source_provider,
            source_config,
            provider_instance_name,
            target,
            query,
        } = request;
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::BROWSE_LIBRARY,
            )
            .await?;
        if source_config.provider() != source_provider {
            return Err(Error::InvalidInput(
                "Preview source provider does not match source_config".to_string(),
            ));
        }
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id,
            creator_id: Some(user_id),
            browse_access_mode: crate::models::PlaylistBrowseAccessMode::Default,
            name: "Preview".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider: Some(source_provider),
            source_config: Some(source_config),
            provider_instance_name,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
            version: 0,
        };
        let (provider_name, provider) = self.get_dynamic_playlist_provider(&playlist).await?;
        self.ensure_provider_credential_repo(&provider_name)?;
        let prepared = PreparedDynamicPlaylist {
            playlist,
            provider_name,
            provider,
        };
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::User(user_id));
        prepared
            .provider
            .validate_source_config(
                &ctx,
                crate::provider::SourceConfig::DynamicPlaylist(
                    prepared
                        .playlist
                        .source_config
                        .as_ref()
                        .ok_or_else(|| Error::InvalidInput("Missing source_config".to_string()))?,
                ),
            )
            .await
            .map_err(Error::from)?;
        prepared
            .dynamic_playlist_provider()?
            .list_playlist(&ctx, &prepared.playlist, target.as_ref(), query)
            .await
            .map_err(Error::from)
    }

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
        let source_provider = dynamic_playlist_source_provider(playlist)?;
        let provider_name = source_provider.as_str().to_string();

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
            .resolve_provider(source_provider, bound_instance)
            .await?;

        Ok((provider_name, provider))
    }

    pub(super) fn dynamic_playlist_context<'a>(
        &'a self,
        prepared: &'a PreparedDynamicPlaylist,
        actor: ProviderActor,
    ) -> ProviderContext<'a> {
        let credential_owner_id = prepared.playlist.creator_id.or(match actor {
            ProviderActor::User(user_id) => Some(user_id),
            ProviderActor::System | ProviderActor::Guest => None,
        });
        self.build_provider_context(
            prepared.provider_name.as_str(),
            actor,
            prepared.playlist.room_id,
            credential_owner_id,
            prepared.playlist.provider_instance_name.as_deref(),
        )
        .with_playlist_id(prepared.playlist.id)
    }

    pub async fn admin_list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&crate::models::ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::User(admin_user_id));

        prepared
            .dynamic_playlist_provider()?
            .list_playlist(&ctx, &prepared.playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn admin_get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        admin_user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::User(admin_user_id));

        prepared
            .dynamic_playlist_provider()?
            .browse_path(&ctx, &prepared.playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn list_dynamic_playlist_items(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&crate::models::ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult> {
        self.permission_service
            .check_permission(
                &room_id,
                &user_id,
                crate::models::RoomPermission::BROWSE_LIBRARY,
            )
            .await?;

        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::User(user_id));

        prepared
            .dynamic_playlist_provider()?
            .list_playlist(&ctx, &prepared.playlist, target, query)
            .await
            .map_err(Error::from)
    }

    pub async fn get_dynamic_playlist_browse_path(
        &self,
        room_id: RoomId,
        user_id: UserId,
        playlist_id: &PlaylistId,
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::User(user_id));

        prepared
            .dynamic_playlist_provider()?
            .browse_path(&ctx, &prepared.playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn resolve_dynamic_playlist_item(
        &self,
        room_id: RoomId,
        actor: ProviderActor,
        playlist_id: &PlaylistId,
        target: &crate::models::ProviderTarget,
    ) -> Result<Option<NextPlayItem>> {
        let prepared = self.prepare_dynamic_playlist(&room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, actor);

        prepared
            .dynamic_playlist_provider()?
            .resolve_item(&ctx, &prepared.playlist, target)
            .await
            .map_err(Error::from)
    }

    pub async fn next_dynamic_playlist_item(
        &self,
        room_id: &RoomId,
        playlist_id: &PlaylistId,
        target: &crate::models::ProviderTarget,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>> {
        let prepared = self.prepare_dynamic_playlist(room_id, playlist_id).await?;
        let ctx = self.dynamic_playlist_context(&prepared, ProviderActor::System);

        prepared
            .dynamic_playlist_provider()?
            .next(&ctx, &prepared.playlist, target, play_mode)
            .await
            .map_err(Error::from)
    }
}

fn dynamic_playlist_source_provider(playlist: &Playlist) -> Result<SourceProvider> {
    let provider = playlist.source_provider.ok_or_else(|| {
        Error::Internal(format!("Dynamic playlist {} missing provider", playlist.id))
    })?;
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
    use crate::models::{Playlist, SourceProvider};

    fn dynamic_playlist(
        source_provider: Option<SourceProvider>,
        source_config: Option<crate::models::PlaylistSourceConfig>,
    ) -> Playlist {
        Playlist {
            id: PlaylistId::expect_positive(10),
            room_id: RoomId::expect_positive(20),
            creator_id: Some(UserId::expect_positive(30)),
            browse_access_mode: crate::models::PlaylistBrowseAccessMode::Default,
            name: "Dynamic".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider,
            source_config,
            provider_instance_name: None,
            created_at: crate::SystemClock.now(),
            updated_at: crate::SystemClock.now(),
            version: 0,
        }
    }

    #[test]
    fn dynamic_playlist_source_provider_returns_valid_provider() {
        let playlist = dynamic_playlist(
            Some(SourceProvider::Alist),
            Some(crate::models::PlaylistSourceConfig::Alist(
                crate::models::AlistPlaylistSourceConfig {
                    server_id: "srv".to_string(),
                    path: "/movies".to_string(),
                    password: None,
                    proxy_mode: crate::models::PlaybackProxyMode::Auto,
                },
            )),
        );

        assert_eq!(
            dynamic_playlist_source_provider(&playlist)
                .expect("dynamic playlist provider should parse"),
            SourceProvider::Alist
        );
    }

    #[test]
    fn dynamic_playlist_source_provider_rejects_missing_provider() {
        let playlist = dynamic_playlist(
            None,
            Some(crate::models::PlaylistSourceConfig::Alist(
                crate::models::AlistPlaylistSourceConfig {
                    server_id: "srv".to_string(),
                    path: "/movies".to_string(),
                    password: None,
                    proxy_mode: crate::models::PlaybackProxyMode::Auto,
                },
            )),
        );

        assert!(matches!(
            dynamic_playlist_source_provider(&playlist),
            Err(Error::Internal(message))
                if message.contains("Dynamic playlist")
                    && message.contains("provider")
        ));
    }

    #[test]
    fn dynamic_playlist_source_provider_rejects_missing_source_config() {
        let playlist = dynamic_playlist(Some(SourceProvider::Alist), None);

        assert!(matches!(
            dynamic_playlist_source_provider(&playlist),
            Err(Error::Internal(message))
                if message.contains("Dynamic playlist")
                    && message.contains("source_config")
        ));
    }
}
