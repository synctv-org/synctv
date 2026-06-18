// Provider Context
// Contains all information needed for provider execution

use sqlx::PgPool;
use std::sync::Arc;
use synctv_common::ExecutionControl;

use crate::credential_encryption::CredentialEncryption;
use crate::models::{MediaId, RoomId, UserId};
use crate::proxy_signature::ProxySigningKey;
use crate::repository::UserProviderCredentialRepository;

use super::{PlaybackClientProfile, ProviderAccessService};

/// Provider execution context.
#[derive(Clone)]
pub struct ProviderContext<'a> {
    /// User ID requesting playback (optional)
    pub user_id: Option<UserId>,

    /// Externally visible user ID for signed proxy URLs.
    pub public_user_id: Option<String>,

    /// User ID whose provider credentials should be used when provider semantics
    /// require creator-owned shared credentials.
    pub credential_owner_id: Option<UserId>,

    /// Externally visible credential owner ID for URLs returned to clients.
    pub public_credential_owner_id: Option<String>,

    /// Room ID (optional)
    pub room_id: Option<RoomId>,

    /// Externally visible room ID for signed proxy URLs.
    pub public_room_id: Option<String>,

    /// Media ID currently being resolved (optional)
    pub media_id: Option<MediaId>,

    /// Bound provider instance name selected by the media/playlist owner (optional)
    pub provider_instance_name: Option<&'a str>,

    /// Base URL for generating proxy URLs
    pub base_url: Option<&'a str>,

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

    /// Proxy signing key for generating HMAC-signed proxy URLs (optional)
    pub signing_key: Option<&'a ProxySigningKey>,

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
            public_user_id: None,
            credential_owner_id: None,
            public_credential_owner_id: None,
            room_id: None,
            public_room_id: None,
            media_id: None,
            provider_instance_name: None,
            base_url: None,
            key_prefix,
            db: None,
            credential_encryption: None,
            store: None,
            credential_repo: None,
            provider_access_service: None,
            signing_key: None,
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

    /// Set externally visible user ID for proxy signatures.
    #[must_use]
    pub fn with_public_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.public_user_id = Some(user_id.into());
        self
    }

    /// Set credential owner ID
    #[must_use]
    pub const fn with_credential_owner_id(mut self, credential_owner_id: UserId) -> Self {
        self.credential_owner_id = Some(credential_owner_id);
        self
    }

    /// Set externally visible credential owner ID for client-facing URLs.
    #[must_use]
    pub fn with_public_credential_owner_id(
        mut self,
        credential_owner_id: impl Into<String>,
    ) -> Self {
        self.public_credential_owner_id = Some(credential_owner_id.into());
        self
    }

    /// Set room ID
    #[must_use]
    pub const fn with_room_id(mut self, room_id: RoomId) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Set externally visible room ID for proxy signatures.
    #[must_use]
    pub fn with_public_room_id(mut self, room_id: impl Into<String>) -> Self {
        self.public_room_id = Some(room_id.into());
        self
    }

    /// Set media ID
    #[must_use]
    pub const fn with_media_id(mut self, media_id: MediaId) -> Self {
        self.media_id = Some(media_id);
        self
    }

    /// Set canonical bound provider instance name.
    #[must_use]
    pub const fn with_provider_instance_name(mut self, provider_instance_name: &'a str) -> Self {
        self.provider_instance_name = Some(provider_instance_name);
        self
    }

    /// Set base URL
    #[must_use]
    pub const fn with_base_url(mut self, base_url: &'a str) -> Self {
        self.base_url = Some(base_url);
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

    /// Set proxy signing key for generating HMAC-signed proxy URLs
    #[must_use]
    pub const fn with_signing_key(mut self, key: &'a ProxySigningKey) -> Self {
        self.signing_key = Some(key);
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
    pub fn proxy_user_id(&self) -> Option<String> {
        self.public_user_id
            .clone()
            .or_else(|| self.user_id.map(|id| id.to_string()))
    }

    #[must_use]
    pub fn proxy_room_id(&self) -> Option<String> {
        self.public_room_id
            .clone()
            .or_else(|| self.room_id.map(|id| id.to_string()))
    }

    #[must_use]
    pub const fn media_id(&self) -> Option<&MediaId> {
        self.media_id.as_ref()
    }

    #[must_use]
    pub const fn credential_owner_id(&self) -> Option<&UserId> {
        self.credential_owner_id.as_ref()
    }

    #[must_use]
    pub fn public_credential_owner_id(&self) -> Option<&str> {
        self.public_credential_owner_id.as_deref()
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

    /// Validate that all required fields are present for playback generation.
    ///
    /// Returns an error listing missing required fields (`base_url`, `db`).
    /// Optional fields (`user_id`, `room_id`, `media_id`, `store`) are not checked.
    pub fn validate(&self) -> Result<(), crate::Error> {
        let mut missing = Vec::new();
        if self.base_url.is_none() {
            missing.push("base_url");
        }
        if self.db.is_none() {
            missing.push("db");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(format!(
                "ProviderContext missing required fields: {}",
                missing.join(", ")
            )))
        }
    }
}
