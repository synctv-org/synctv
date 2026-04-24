// Provider Context
// Contains all information needed for provider execution

use sqlx::PgPool;
use std::sync::Arc;
use synctv_common::ExecutionControl;

use crate::repository::UserProviderCredentialRepository;
use crate::service::proxy_signature::ProxySigningKey;
use crate::service::CredentialEncryption;

use super::PlaybackClientProfile;

/// Provider execution context
///
/// Provides access to database, shared provider storage, user information, and other resources
/// needed by providers to generate playback information.
#[derive(Clone)]
pub struct ProviderContext<'a> {
    /// User ID requesting playback (optional)
    pub user_id: Option<&'a str>,

    /// Room ID (optional)
    pub room_id: Option<&'a str>,

    /// Media ID currently being resolved (optional)
    pub media_id: Option<&'a str>,

    /// Bound provider instance name selected by the media/playlist owner (optional)
    pub provider_instance_name: Option<&'a str>,

    /// Base URL for generating proxy URLs
    pub base_url: Option<&'a str>,

    /// Cache key prefix (e.g., "synctv")
    pub key_prefix: &'a str,

    /// Database connection pool (optional)
    pub db: Option<&'a PgPool>,

    /// Credential encryption for protecting sensitive data in `source_config` (optional)
    pub credential_encryption: Option<&'a CredentialEncryption>,

    /// Provider store for caching and distributed locking (optional)
    pub store: Option<Arc<dyn super::store::ProviderStore>>,

    /// User provider credential repository for resolving stored credentials (optional)
    pub credential_repo: Option<&'a UserProviderCredentialRepository>,

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
            room_id: None,
            media_id: None,
            provider_instance_name: None,
            base_url: None,
            key_prefix,
            db: None,
            credential_encryption: None,
            store: None,
            credential_repo: None,
            signing_key: None,
            request_context: None,
            playback_client_profile: None,
        }
    }

    /// Set user ID
    #[must_use]
    pub const fn with_user_id(mut self, user_id: &'a str) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Set room ID
    #[must_use]
    pub const fn with_room_id(mut self, room_id: &'a str) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Set media ID
    #[must_use]
    pub const fn with_media_id(mut self, media_id: &'a str) -> Self {
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

    /// Set credential encryption for protecting sensitive data in `source_config`
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

    /// Set user provider credential repository for resolving stored credentials
    #[must_use]
    pub const fn with_credential_repo(
        mut self,
        repo: &'a UserProviderCredentialRepository,
    ) -> Self {
        self.credential_repo = Some(repo);
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
