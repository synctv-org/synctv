use std::sync::Arc;

use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    cache::{
        CacheInvalidationRuntime, ConsistencyCoordinator, KeyBuilder, UsernameCache,
        VersionFenceStore,
    },
    repository::{
        realtime_outbox::RealtimeOutboxRepository, EmailBindRepository,
        EmailRegistrationTokenRepository, TotpCredentialRepository, UserEmailRepository,
        UserPasswordRepository, UserPreferencesRepository, UserRepository,
    },
    service::{file_storage::FileStorageService, PermissionService, RequestRateLimiterService},
    service::{
        BruteForceProtectionService, JwtService, OpaquePasswordService, TokenBlacklistStore,
    },
    validation::PasswordComplexityOptions,
};

const REFRESH_RATE_LIMIT_REQUESTS: u32 = 10;
const REFRESH_RATE_LIMIT_WINDOW_SECS: u64 = 60;

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    value.max(0).cast_unsigned()
}

#[derive(Debug, Clone, Copy)]
pub struct RefreshRateLimitConfig {
    pub requests: u32,
    pub window_secs: u64,
}

impl Default for RefreshRateLimitConfig {
    fn default() -> Self {
        Self {
            requests: REFRESH_RATE_LIMIT_REQUESTS,
            window_secs: REFRESH_RATE_LIMIT_WINDOW_SECS,
        }
    }
}

#[derive(Clone)]
pub struct UserService {
    pub(crate) repository: UserRepository,
    pub(crate) user_email_repository: UserEmailRepository,
    pub(crate) user_password_repository: UserPasswordRepository,
    email_bind_repository: EmailBindRepository,
    email_registration_token_repository: EmailRegistrationTokenRepository,
    pub(crate) user_preferences_repository: UserPreferencesRepository,
    pub(crate) totp_credential_repository: TotpCredentialRepository,
    jwt_service: JwtService,
    username_cache: UsernameCache,
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    brute_force: Arc<dyn BruteForceProtectionService>,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    key_builder: KeyBuilder,
    refresh_rate_limiter: Arc<dyn RequestRateLimiterService>,
    refresh_rate_limit_config: RefreshRateLimitConfig,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
    password_registration_policy_override: Option<RegistrationPolicy>,
    password_complexity: PasswordComplexityOptions,
    opaque_password_service: Arc<OpaquePasswordService>,
    login_discovery_key: [u8; 32],
    login_session_store: Arc<dyn LoginSessionStore>,
    opaque_registration_session_store: Arc<dyn OpaqueRegistrationSessionStore>,
    mfa_session_store: Arc<dyn MfaSessionStore>,
    sensitive_verification_session_store: Arc<dyn SensitiveVerificationSessionStore>,
    permission_service: Option<PermissionService>,
    consistency: ConsistencyCoordinator,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
    credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
}

pub struct UserServiceRuntimeOptions {
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub refresh_rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub refresh_rate_limit_config: RefreshRateLimitConfig,
    pub runtime_settings_store: Option<Arc<crate::service::RuntimeSettingsStore>>,
    pub password_registration_policy_override: Option<RegistrationPolicy>,
    /// Stable OPAQUE server setup used for password registration, login, and reset.
    ///
    /// Composition roots for real deployments must inject a service derived from
    /// the configured `security.opaque_server_setup_secret`.
    pub opaque_password_service: Arc<OpaquePasswordService>,
    pub login_discovery_key: [u8; 32],
    pub login_session_store: Arc<dyn LoginSessionStore>,
    pub opaque_registration_session_store: Arc<dyn OpaqueRegistrationSessionStore>,
    pub mfa_session_store: Arc<dyn MfaSessionStore>,
    pub sensitive_verification_session_store: Arc<dyn SensitiveVerificationSessionStore>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub permission_service: Option<PermissionService>,
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
    pub read_pool: Option<PgPool>,
    pub credential_encryption: Option<crate::credential_encryption::CredentialEncryption>,
}

impl UserServiceRuntimeOptions {
    #[must_use]
    pub fn derive_login_discovery_key(secret: &[u8]) -> [u8; 32] {
        Sha256::digest([b"synctv:login-discovery-profile:v1:".as_slice(), secret].concat()).into()
    }

    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_defaults() -> Self {
        Self {
            cache_invalidation: None,
            refresh_rate_limiter: Arc::new(crate::service::RateLimiter::local_only(
                "synctv:test:".to_string(),
            )),
            refresh_rate_limit_config: RefreshRateLimitConfig::default(),
            runtime_settings_store: None,
            password_registration_policy_override: None,
            opaque_password_service: Arc::new(OpaquePasswordService::new_ephemeral_for_process()),
            login_discovery_key: Self::derive_login_discovery_key(
                b"synctv-test-login-discovery-secret",
            ),
            login_session_store: crate::service::local_login_session_store(),
            opaque_registration_session_store:
                crate::service::local_opaque_registration_session_store(),
            mfa_session_store: crate::service::local_mfa_session_store(),
            sensitive_verification_session_store:
                crate::service::local_sensitive_verification_session_store(),
            realtime_outbox: None,
            permission_service: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            file_storage_service: None,
            read_pool: None,
            credential_encryption: Some(
                crate::credential_encryption::CredentialEncryption::new(&[0x42; 32])
                    .expect("fixed test credential encryption key is valid"),
            ),
        }
    }
}

pub struct UserServiceDependencies {
    pub jwt_service: JwtService,
    pub username_cache: UsernameCache,
    pub token_blacklist: Arc<dyn TokenBlacklistStore>,
    pub key_builder: KeyBuilder,
    pub brute_force: Arc<dyn BruteForceProtectionService>,
    pub password_complexity: PasswordComplexityOptions,
}

impl std::fmt::Debug for UserService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserService")
            .field("username_cache", &self.username_cache)
            .finish()
    }
}

mod avatar;
mod constructor;
mod deletion;
pub use deletion::{
    UserDeletedChatMessage, UserDeletedRoomImpact, UserDeletionOptions, UserDeletionSource,
    UserDeletionSummary,
};
mod identity_bindings;
mod identity_policy;
mod login;
mod lookup;
mod oauth2_users;
mod password_credentials;
mod profile;
mod recovery;
pub use recovery::{UserRestoreOptions, UserRestoreResult};
mod registration_auth;
mod registration_review;
mod registration_types;
mod session_stores;
mod ticket_validation;
mod username_cache;
mod verification;
use identity_policy::password_binding;
pub(crate) use registration_types::PendingRegistrationConflict;
pub use registration_types::{
    AccountRegistrationOutcome, CreateUserAvatarUploadSession, PendingAccountRegistration,
    RegistrationMode, RegistrationPolicy,
};
mod session_types;
pub use session_types::{
    AuthFactorMethod, AuthenticatedLogin, LoginSession, LoginSessionState, LoginStartChallenge,
    MfaChallenge, MfaSession, OpaqueLoginStartChallenge, OpaquePasswordUpdateVerification,
    OpaqueRegistrationPurpose, OpaqueRegistrationSession, OpaqueRegistrationStartChallenge,
    SensitiveVerificationChallenge, SensitiveVerificationOutcome, SensitiveVerificationSession,
};
mod tokens;
mod totp;
pub use session_stores::{
    local_login_session_store, local_mfa_session_store, local_opaque_registration_session_store,
    local_sensitive_verification_session_store,
};
pub use session_stores::{
    login_session_store_from_shared_state_profile, mfa_session_store_from_shared_state_profile,
    opaque_registration_session_store_from_shared_state_profile,
    sensitive_verification_session_store_from_shared_state_profile,
};
pub use session_stores::{
    InMemoryLoginSessionStore, InMemoryMfaSessionStore, InMemoryOpaqueRegistrationSessionStore,
    InMemorySensitiveVerificationSessionStore, LoginSessionStore, MfaSessionStore,
    OpaqueRegistrationSessionStore, SensitiveVerificationSessionStore,
};
pub use totp::{TotpRecoveryCodes, TotpSetup};

#[cfg(test)]
mod tests;
