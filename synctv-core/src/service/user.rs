use std::sync::Arc;

use crate::{
    cache::{
        CacheInvalidationRuntime, ConsistencyCoordinator, KeyBuilder, UsernameCache,
        VersionFenceStore,
    },
    config::PasswordComplexityConfig,
    repository::{
        realtime_outbox::RealtimeOutboxRepository, EmailBindRepository,
        EmailRegistrationTokenRepository, UserEmailRepository, UserPasswordRepository,
        UserPreferencesRepository, UserRepository,
    },
    service::auth::{
        BruteForceProtectionService, JwtService, OpaquePasswordService, TokenBlacklistStore,
    },
    service::rate_limit::RequestRateLimiterService,
    service::{file_storage::FileStorageService, PermissionService},
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
    jwt_service: JwtService,
    username_cache: UsernameCache,
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    brute_force: Arc<dyn BruteForceProtectionService>,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    key_builder: KeyBuilder,
    refresh_rate_limiter: Arc<dyn RequestRateLimiterService>,
    refresh_rate_limit_config: RefreshRateLimitConfig,
    realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
    password_registration_policy_override: Option<RegistrationPolicy>,
    password_complexity: PasswordComplexityConfig,
    opaque_password_service: Arc<OpaquePasswordService>,
    opaque_login_session_store: Arc<dyn OpaqueLoginSessionStore>,
    opaque_registration_session_store: Arc<dyn OpaqueRegistrationSessionStore>,
    mfa_session_store: Arc<dyn MfaSessionStore>,
    sensitive_verification_session_store: Arc<dyn SensitiveVerificationSessionStore>,
    permission_service: Option<PermissionService>,
    consistency: ConsistencyCoordinator,
    file_storage_service: Option<Arc<dyn FileStorageService>>,
}

pub struct UserServiceRuntimeOptions {
    pub cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    pub refresh_rate_limiter: Arc<dyn RequestRateLimiterService>,
    pub refresh_rate_limit_config: RefreshRateLimitConfig,
    pub settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
    pub password_registration_policy_override: Option<RegistrationPolicy>,
    /// Stable OPAQUE server setup used for password registration, login, and reset.
    ///
    /// Composition roots for real deployments must inject a service derived from
    /// the configured `security.opaque_server_setup_secret`.
    pub opaque_password_service: Arc<OpaquePasswordService>,
    pub opaque_login_session_store: Arc<dyn OpaqueLoginSessionStore>,
    pub opaque_registration_session_store: Arc<dyn OpaqueRegistrationSessionStore>,
    pub mfa_session_store: Arc<dyn MfaSessionStore>,
    pub sensitive_verification_session_store: Arc<dyn SensitiveVerificationSessionStore>,
    pub realtime_outbox: Option<Arc<RealtimeOutboxRepository>>,
    pub permission_service: Option<PermissionService>,
    pub version_fence: Arc<dyn VersionFenceStore>,
    pub file_storage_service: Option<Arc<dyn FileStorageService>>,
}

impl UserServiceRuntimeOptions {
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_defaults() -> Self {
        Self {
            cache_invalidation: None,
            refresh_rate_limiter: Arc::new(crate::service::rate_limit::RateLimiter::local_only(
                "synctv:test:".to_string(),
            )),
            refresh_rate_limit_config: RefreshRateLimitConfig::default(),
            settings_registry: None,
            password_registration_policy_override: None,
            opaque_password_service: Arc::new(OpaquePasswordService::new_ephemeral_for_process()),
            opaque_login_session_store: crate::service::user::local_opaque_login_session_store(),
            opaque_registration_session_store:
                crate::service::user::local_opaque_registration_session_store(),
            mfa_session_store: crate::service::user::local_mfa_session_store(),
            sensitive_verification_session_store:
                crate::service::user::local_sensitive_verification_session_store(),
            realtime_outbox: None,
            permission_service: None,
            version_fence: Arc::new(crate::cache::LocalVersionFenceStore::new()),
            file_storage_service: None,
        }
    }
}

pub struct UserServiceDependencies {
    pub jwt_service: JwtService,
    pub username_cache: UsernameCache,
    pub token_blacklist: Arc<dyn TokenBlacklistStore>,
    pub key_builder: KeyBuilder,
    pub brute_force: Arc<dyn BruteForceProtectionService>,
    pub password_complexity: PasswordComplexityConfig,
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
pub use deletion::{UserDeletedRoomImpact, UserDeletionSummary};
mod identity_bindings;
mod identity_policy;
mod login;
mod lookup;
mod oauth2_users;
mod password_credentials;
mod profile;
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
    AuthFactorMethod, AuthenticatedLogin, MfaChallenge, MfaSession, OpaqueLoginSession,
    OpaqueLoginStartChallenge, OpaquePasswordUpdateVerification, OpaqueRegistrationPurpose,
    OpaqueRegistrationSession, OpaqueRegistrationStartChallenge, SensitiveVerificationChallenge,
    SensitiveVerificationOutcome, SensitiveVerificationSession,
};
mod tokens;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use session_stores::{
    local_mfa_session_store, local_opaque_login_session_store,
    local_opaque_registration_session_store, local_sensitive_verification_session_store,
};
pub(crate) use session_stores::{
    mfa_session_store_from_shared_state_profile,
    opaque_login_session_store_from_shared_state_profile,
    opaque_registration_session_store_from_shared_state_profile,
    sensitive_verification_session_store_from_shared_state_profile,
};
pub use session_stores::{
    InMemoryMfaSessionStore, InMemoryOpaqueLoginSessionStore,
    InMemoryOpaqueRegistrationSessionStore, InMemorySensitiveVerificationSessionStore,
    MfaSessionStore, OpaqueLoginSessionStore, OpaqueRegistrationSessionStore,
    SensitiveVerificationSessionStore,
};

#[cfg(test)]
mod tests;
