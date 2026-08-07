// Provider Context
// Contains all information needed for provider execution

use sqlx::PgPool;
use std::sync::Arc;
use synctv_common::ExecutionControl;

use crate::credential_encryption::CredentialEncryption;
use crate::models::{MediaId, RoomId, UserId};
use crate::repository::UserProviderCredentialRepository;

use super::{PlaybackClientProfile, ProviderAccessService};

/// Provider execution context.
#[derive(Clone)]
pub struct ProviderContext<'a> {
    /// User ID requesting playback (optional)
    pub user_id: Option<UserId>,

    /// User ID whose provider credentials should be used when provider semantics
    /// require creator-owned shared credentials.
    pub credential_owner_id: Option<UserId>,

    /// Room ID (optional)
    pub room_id: Option<RoomId>,

    /// Media ID currently being resolved (optional)
    pub media_id: Option<MediaId>,

    /// Playback generation that owns provider-side resources allocated by this request.
    pub playback_generation: Option<i64>,

    /// Whether the owning room playback state is currently playing.
    pub playback_is_playing: Option<bool>,

    /// Bound provider instance name selected by the media/playlist owner (optional)
    pub provider_instance_name: Option<&'a str>,

    /// Cache key prefix (e.g., "synctv")
    pub key_prefix: &'a str,

    /// Database connection pool (optional)
    pub db: Option<&'a PgPool>,

    /// Credential encryption required by credential-backed providers.
    pub credential_encryption: Option<&'a CredentialEncryption>,

    /// Provider store for caching and distributed locking (optional)
    pub store: Option<Arc<dyn super::store::ProviderStore>>,

    /// Repository required by credential-backed providers.
    pub credential_repo: Option<&'a UserProviderCredentialRepository>,

    /// Typed provider access service for cached credential/session resolution.
    pub provider_access_service: Option<Arc<dyn ProviderAccessService>>,

    /// Cooperative request context propagated from the caller, if any.
    pub request_context: Option<ExecutionControl>,

    /// Client playback capability hints for request-scoped media negotiation.
    pub playback_client_profile: Option<PlaybackClientProfile>,
}

impl<'a> ProviderContext<'a> {
    /// Create new context with defaults
    #[must_use]
    pub fn new(key_prefix: &'a str) -> Self {
        Self {
            user_id: None,
            credential_owner_id: None,
            room_id: None,
            media_id: None,
            playback_generation: None,
            playback_is_playing: None,
            provider_instance_name: None,
            key_prefix,
            db: None,
            credential_encryption: None,
            store: None,
            credential_repo: None,
            provider_access_service: None,
            request_context: None,
            playback_client_profile: None,
        }
    }

    /// Set user ID
    #[must_use]
    pub const fn with_user_id(mut self, user_id: UserId) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Set credential owner ID
    #[must_use]
    pub const fn with_credential_owner_id(mut self, credential_owner_id: UserId) -> Self {
        self.credential_owner_id = Some(credential_owner_id);
        self
    }

    /// Set room ID
    #[must_use]
    pub const fn with_room_id(mut self, room_id: RoomId) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Set media ID
    #[must_use]
    pub const fn with_media_id(mut self, media_id: MediaId) -> Self {
        self.media_id = Some(media_id);
        self
    }

    /// Bind provider resources allocated by this request to a playback generation.
    #[must_use]
    pub const fn with_playback_generation(mut self, playback_generation: i64) -> Self {
        self.playback_generation = Some(playback_generation);
        self
    }

    /// Attach the current room playback activity state.
    #[must_use]
    pub const fn with_playback_is_playing(mut self, playback_is_playing: bool) -> Self {
        self.playback_is_playing = Some(playback_is_playing);
        self
    }

    /// Set canonical bound provider instance name.
    #[must_use]
    pub const fn with_provider_instance_name(mut self, provider_instance_name: &'a str) -> Self {
        self.provider_instance_name = Some(provider_instance_name);
        self
    }

    /// Set database pool
    #[must_use]
    pub const fn with_db(mut self, db: &'a PgPool) -> Self {
        self.db = Some(db);
        self
    }

    /// Set credential encryption for credential-backed provider resolution.
    #[must_use]
    pub const fn with_credential_encryption(mut self, enc: &'a CredentialEncryption) -> Self {
        self.credential_encryption = Some(enc);
        self
    }

    /// Set provider store for caching and distributed locking
    #[must_use]
    pub fn with_store(mut self, store: Arc<dyn super::store::ProviderStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Set repository for credential-backed provider resolution.
    #[must_use]
    pub const fn with_credential_repo(
        mut self,
        repo: &'a UserProviderCredentialRepository,
    ) -> Self {
        self.credential_repo = Some(repo);
        self
    }

    /// Set typed provider access service for cached credential/session resolution
    #[must_use]
    pub fn with_provider_access_service(mut self, service: Arc<dyn ProviderAccessService>) -> Self {
        self.provider_access_service = Some(service);
        self
    }

    /// Attach the caller's cooperative request context for downstream provider I/O.
    #[must_use]
    pub fn with_request_context(mut self, request_context: Option<ExecutionControl>) -> Self {
        self.request_context = request_context;
        self
    }

    #[must_use]
    pub fn with_playback_client_profile(
        mut self,
        playback_client_profile: Option<PlaybackClientProfile>,
    ) -> Self {
        self.playback_client_profile = playback_client_profile;
        self
    }

    #[must_use]
    pub const fn request_context(&self) -> Option<&ExecutionControl> {
        self.request_context.as_ref()
    }

    #[must_use]
    pub const fn playback_client_profile(&self) -> Option<&PlaybackClientProfile> {
        self.playback_client_profile.as_ref()
    }

    #[must_use]
    pub const fn provider_instance_name(&self) -> Option<&str> {
        self.provider_instance_name
    }

    #[must_use]
    pub const fn user_id(&self) -> Option<&UserId> {
        self.user_id.as_ref()
    }

    #[must_use]
    pub const fn room_id(&self) -> Option<&RoomId> {
        self.room_id.as_ref()
    }

    #[must_use]
    pub const fn media_id(&self) -> Option<&MediaId> {
        self.media_id.as_ref()
    }

    #[must_use]
    pub const fn playback_generation(&self) -> Option<i64> {
        self.playback_generation
    }

    #[must_use]
    pub const fn playback_is_playing(&self) -> Option<bool> {
        self.playback_is_playing
    }

    #[must_use]
    pub const fn credential_owner_id(&self) -> Option<&UserId> {
        self.credential_owner_id.as_ref()
    }

    #[must_use]
    pub fn credential_owner_or_user_id(&self) -> Option<&UserId> {
        self.credential_owner_id().or_else(|| self.user_id())
    }

    pub fn check_active(&self) -> Result<(), crate::Error> {
        if let Some(request_context) = self.request_context.as_ref() {
            request_context
                .check_active()
                .map_err(|err| crate::Error::Timeout(err.to_string()))?;
        }

        Ok(())
    }
}
