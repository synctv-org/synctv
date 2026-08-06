//! Playlist management service
//!
//! Design reference: external design doc 04-database-design.md §2.4.1
//!
//! Manages playlist operations including:
//! - Creating static and provider-backed playlists
//! - Tree structure navigation
//! - Position management

use std::sync::Arc;

use crate::{
    models::{Playlist, UserId},
    repository::realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
    repository::PlaylistRepository,
    repository::UserProviderCredentialRepository,
    service::{FileStorageService, PermissionService, ProvidersManager},
    Error, Result,
};

mod cover;
mod creation;
mod dynamic;
mod movement;
mod properties;
mod queries;
pub use cover::CreatePlaylistCoverUploadSession;
pub use creation::CreatePlaylistRequest;
#[cfg(test)]
use dynamic::{
    normalize_dynamic_playlist_fields, validate_dynamic_playlist_source_with_dependencies,
    DynamicPlaylistValidationDeps,
};
pub use movement::MovePlaylistRequest;
pub use properties::SetPlaylistRequest;

pub type RealtimeOutboxPlaylistEventFactory =
    Arc<dyn Fn(&Playlist) -> Result<NewRealtimeOutboxEvent> + Send + Sync>;

fn ensure_playlist_creator_can_edit(playlist: &Playlist, user_id: &UserId) -> Result<()> {
    if playlist.creator_id.as_ref() == Some(user_id) {
        Ok(())
    } else {
        Err(Error::Authorization(
            "Only the playlist creator can edit playlists".to_string(),
        ))
    }
}

/// Playlist management service
///
/// Responsible for playlist operations:
/// - Create static playlists for manually added media
/// - Create provider-backed dynamic playlists
/// - Tree structure navigation
#[derive(Clone)]
pub struct PlaylistService {
    playlist_repo: PlaylistRepository,
    permission_service: PermissionService,
    providers_manager: Arc<ProvidersManager>,
    /// Credential encryption used by credential-backed providers.
    credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
    /// Repository used by credential-backed providers.
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl std::fmt::Debug for PlaylistService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaylistService").finish()
    }
}

impl PlaylistService {
    async fn insert_playlist_outbox_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        playlist: &Playlist,
        outbox_event_factory: Option<&RealtimeOutboxPlaylistEventFactory>,
    ) -> Result<()> {
        if let Some(event) = outbox_event_factory
            .map(|factory| factory(playlist))
            .transpose()?
        {
            if let Some(outbox) = &self.realtime_outbox {
                outbox.insert_with_executor(&event, &mut **tx).await?;
            }
        }
        Ok(())
    }

    /// Create a new playlist service
    #[must_use]
    pub fn new(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
    ) -> Self {
        Self {
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption: None,
            credential_repo: None,
            realtime_outbox: None,
            file_storage_service: None,
        }
    }

    /// Create a playlist service with provider credential dependencies already wired.
    #[must_use]
    pub fn new_with_provider_credentials(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
        credential_repo: Option<Arc<UserProviderCredentialRepository>>,
    ) -> Self {
        Self::new_with_runtime(
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption,
            credential_repo,
            None,
            None,
        )
    }

    #[must_use]
    pub fn new_with_runtime(
        playlist_repo: PlaylistRepository,
        permission_service: PermissionService,
        providers_manager: Arc<ProvidersManager>,
        credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
        credential_repo: Option<Arc<UserProviderCredentialRepository>>,
        realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
        file_storage_service: Option<Arc<dyn FileStorageService>>,
    ) -> Self {
        assert!(
            credential_repo.is_none() || credential_encryption.is_some(),
            "provider credential repository wiring requires credential encryption"
        );
        Self {
            playlist_repo,
            permission_service,
            providers_manager,
            credential_encryption,
            credential_repo,
            realtime_outbox,
            file_storage_service,
        }
    }

    #[must_use]
    pub fn file_storage_service(&self) -> Option<&Arc<dyn FileStorageService>> {
        self.file_storage_service.as_ref()
    }

    /// Get a reference to the providers manager.
    #[must_use]
    pub const fn providers_manager(&self) -> &Arc<ProvidersManager> {
        &self.providers_manager
    }
}

#[cfg(test)]
mod tests;
