use sqlx::PgPool;
use std::sync::Arc;

use crate::{
    cache::ConsistencyCoordinator,
    config::PasswordComplexityConfig,
    repository::{
        EmailBindRepository, EmailRegistrationTokenRepository, UserEmailRepository,
        UserPasswordRepository, UserPreferencesRepository, UserRepository,
    },
    service::{
        auth::{BruteForceProtectionService, JwtService, TokenBlacklistStore},
        user::{UserService, UserServiceDependencies, UserServiceRuntimeOptions},
    },
};

impl UserService {
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_tests(
        pool: &PgPool,
        jwt_service: JwtService,
        username_cache: crate::cache::UsernameCache,
        token_blacklist: Arc<dyn TokenBlacklistStore>,
        key_builder: crate::cache::KeyBuilder,
        brute_force: impl BruteForceProtectionService + 'static,
    ) -> Self {
        Self::new_with_brute_force_service_and_runtime(
            pool,
            UserServiceDependencies {
                jwt_service,
                username_cache,
                token_blacklist,
                key_builder,
                brute_force: Arc::new(brute_force),
                password_complexity: PasswordComplexityConfig::default(),
            },
            UserServiceRuntimeOptions::test_defaults(),
        )
    }

    #[must_use]
    pub fn new_with_runtime(
        pool: &PgPool,
        jwt_service: JwtService,
        username_cache: crate::cache::UsernameCache,
        token_blacklist: Arc<dyn TokenBlacklistStore>,
        key_builder: crate::cache::KeyBuilder,
        brute_force: impl BruteForceProtectionService + 'static,
        runtime: UserServiceRuntimeOptions,
    ) -> Self {
        Self::new_with_brute_force_service_and_runtime(
            pool,
            UserServiceDependencies {
                jwt_service,
                username_cache,
                token_blacklist,
                key_builder,
                brute_force: Arc::new(brute_force),
                password_complexity: PasswordComplexityConfig::default(),
            },
            runtime,
        )
    }

    #[must_use]
    pub fn new_with_brute_force_service_and_runtime(
        pool: &PgPool,
        dependencies: UserServiceDependencies,
        runtime: UserServiceRuntimeOptions,
    ) -> Self {
        let UserServiceDependencies {
            jwt_service,
            username_cache,
            token_blacklist,
            key_builder,
            brute_force,
            password_complexity,
        } = dependencies;
        let repository = runtime.read_pool.clone().map_or_else(
            || UserRepository::new(pool.clone()),
            |read_pool| UserRepository::new_with_read_pool(pool.clone(), read_pool),
        );

        Self {
            repository,
            user_email_repository: UserEmailRepository::new(pool.clone()),
            user_password_repository: UserPasswordRepository::new(pool.clone()),
            email_bind_repository: EmailBindRepository::new(pool.clone()),
            email_registration_token_repository: EmailRegistrationTokenRepository::new(
                pool.clone(),
            ),
            user_preferences_repository: UserPreferencesRepository::new(pool.clone()),
            jwt_service,
            username_cache,
            cache_invalidation: runtime.cache_invalidation,
            brute_force,
            token_blacklist,
            key_builder,
            refresh_rate_limiter: runtime.refresh_rate_limiter,
            refresh_rate_limit_config: runtime.refresh_rate_limit_config,
            realtime_outbox: runtime.realtime_outbox,
            settings_registry: runtime.settings_registry,
            password_registration_policy_override: runtime.password_registration_policy_override,
            password_complexity,
            opaque_password_service: runtime.opaque_password_service,
            opaque_login_session_store: runtime.opaque_login_session_store,
            opaque_registration_session_store: runtime.opaque_registration_session_store,
            mfa_session_store: runtime.mfa_session_store,
            sensitive_verification_session_store: runtime.sensitive_verification_session_store,
            permission_service: runtime.permission_service,
            file_storage_service: runtime.file_storage_service,
            consistency: ConsistencyCoordinator::new(runtime.version_fence),
        }
    }
}
