//! Service factory helpers for tests

use crate::constants;
use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ciphersuite::CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use std::{net::IpAddr, sync::Arc};
use synctv_core::cache::{KeyBuilder, UsernameCache};
use synctv_core::models::{ProviderInstance, ProviderInstanceListQuery, User};
use synctv_core::repository::SettingsRepository;
use synctv_core::service::{
    auth::{jwt::JwtService, token_blacklist::InMemoryTokenBlacklistStore},
    rate_limit::RequestRateLimiterService,
    room::RoomServiceOptions,
    user::{UserServiceDependencies, UserServiceRuntimeOptions},
    AccountRegistrationOutcome, BruteForceProtection, BruteForceProtectionService,
    ProviderInstanceStore, RateLimiter, RemoteProviderManager, RoomService, RuntimeSettingsStore,
    SettingsService, TokenBlacklistStore, UserService,
};

#[derive(Clone)]
struct FailingRedisRuntime;

#[async_trait::async_trait]
impl synctv_core::RedisConnectionRuntime for FailingRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        panic!("failing_redis_runtime snapshot should not be called")
    }
}

pub fn failing_redis_runtime() -> Arc<dyn synctv_core::RedisConnectionRuntime> {
    Arc::new(FailingRedisRuntime)
}

#[derive(Debug)]
struct EmptyProviderInstanceStore;

#[async_trait::async_trait]
impl ProviderInstanceStore for EmptyProviderInstanceStore {
    async fn get_all_enabled(&self) -> synctv_core::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn get_all(&self) -> synctv_core::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn get_by_name(&self, _name: &str) -> synctv_core::Result<Option<ProviderInstance>> {
        Ok(None)
    }

    async fn list_with_total(
        &self,
        _query: &ProviderInstanceListQuery,
    ) -> synctv_core::Result<(Vec<ProviderInstance>, i64)> {
        Ok((Vec::new(), 0))
    }

    async fn find_by_provider(
        &self,
        _provider: &str,
    ) -> synctv_core::Result<Vec<ProviderInstance>> {
        Ok(Vec::new())
    }

    async fn create(&self, _instance: &ProviderInstance) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn update(&self, _instance: &ProviderInstance) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn delete(&self, _name: &str) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn enable(&self, _name: &str) -> synctv_core::Result<()> {
        Ok(())
    }

    async fn disable(&self, _name: &str) -> synctv_core::Result<()> {
        Ok(())
    }
}

#[must_use]
fn create_empty_provider_instance_store() -> Arc<dyn ProviderInstanceStore> {
    Arc::new(EmptyProviderInstanceStore)
}

#[must_use]
pub fn create_empty_provider_instance_manager() -> Arc<RemoteProviderManager> {
    Arc::new(RemoteProviderManager::new_with_store(
        create_empty_provider_instance_store(),
        None,
    ))
}

#[derive(Debug)]
struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2_010::Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

/// Register a user through the public OPAQUE registration protocol.
pub async fn opaque_register_user(
    service: &UserService,
    username: impl Into<String>,
    email: Option<String>,
    password: impl AsRef<str>,
) -> synctv_core::Result<(User, Option<String>, Option<String>)> {
    opaque_register_user_with_client_ip(service, username, email, password, None).await
}

/// Register a user through OPAQUE with an optional source IP.
pub async fn opaque_register_user_with_client_ip(
    service: &UserService,
    username: impl Into<String>,
    email: Option<String>,
    password: impl AsRef<str>,
    client_ip: Option<IpAddr>,
) -> synctv_core::Result<(User, Option<String>, Option<String>)> {
    let username = username.into();
    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_ref().as_bytes())
            .map_err(|error| synctv_core::Error::Internal(error.to_string()))?;

    let start = service
        .start_opaque_registration_with_control(
            username,
            email,
            client_start.message.serialize().to_vec(),
            client_ip,
            None,
        )
        .await?;

    let registration_response =
        RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(&start.registration_response)
            .map_err(|error| synctv_core::Error::Internal(error.to_string()))?;
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_ref().as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .map_err(|error| synctv_core::Error::Internal(error.to_string()))?;

    match service
        .finish_opaque_registration_with_control(
            &start.session_id,
            client_finish.message.serialize().to_vec(),
            None,
            None,
        )
        .await
    {
        Ok(AccountRegistrationOutcome::Registered {
            user,
            access_token,
            refresh_token,
            ..
        }) => Ok((user, Some(access_token), Some(refresh_token))),
        Ok(AccountRegistrationOutcome::PendingReview(_)) => Err(synctv_core::Error::Internal(
            "test OPAQUE registration helper received pending review outcome".to_string(),
        )),
        Err(error) => Err(error),
    }
}

/// Login through the public OPAQUE login protocol.
pub async fn opaque_login_user(
    service: &UserService,
    identifier: impl Into<String>,
    password: impl AsRef<str>,
) -> synctv_core::Result<synctv_core::service::AuthenticatedLogin> {
    let login = opaque_login_user_with_challenge(service, identifier, password).await?;

    match login {
        synctv_core::service::AuthenticatedLogin::Complete { .. } => Ok(login),
        synctv_core::service::AuthenticatedLogin::MfaRequired { .. } => Err(
            synctv_core::Error::Authentication("OPAQUE login requires MFA".to_string()),
        ),
    }
}

/// Login through OPAQUE and return either completed tokens or an MFA challenge.
pub async fn opaque_login_user_with_challenge(
    service: &UserService,
    identifier: impl Into<String>,
    password: impl AsRef<str>,
) -> synctv_core::Result<synctv_core::service::AuthenticatedLogin> {
    let mut rng = OsRng;
    let client_start =
        ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_ref().as_bytes())
            .map_err(|error| synctv_core::Error::Internal(error.to_string()))?;
    let start = service
        .start_opaque_login_with_control(
            identifier.into(),
            client_start.message.serialize().to_vec(),
            None,
            None,
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&start.credential_response)
            .map_err(|error| synctv_core::Error::Internal(error.to_string()))?;
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_ref().as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| synctv_core::Error::Authentication("Authentication failed".to_string()))?;
    let login = service
        .finish_opaque_login_with_control(
            &start.session_id,
            client_finish.message.serialize().to_vec(),
            None,
            None,
        )
        .await?;

    Ok(login)
}

/// Creates a JWT service for testing
///
/// Uses a fixed test secret key. Do not use in production!
///
/// # Example
///
/// ```text
/// use synctv_core_testing::create_test_jwt_service;
///
/// let jwt_service = create_test_jwt_service();
/// let token = jwt_service.sign_token(user_id, UserRole::User, TokenType::Access)?;
/// ```
#[must_use]
pub fn create_test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

/// Creates a brute-force protection service trait object for testing.
#[must_use]
pub fn create_test_brute_force_protection_service() -> Arc<dyn BruteForceProtectionService> {
    Arc::new(BruteForceProtection::in_memory("test".to_string()))
}

fn create_test_token_blacklist_store() -> InMemoryTokenBlacklistStore {
    InMemoryTokenBlacklistStore::new(
        constants::token_blacklist::CAPACITY as u64,
        constants::token_blacklist::SHORT_TTL_SECS,
        constants::token_blacklist::LONG_TTL_SECS,
    )
}

/// Creates a token blacklist store trait object for testing.
#[must_use]
pub fn create_test_token_blacklist_store_service() -> Arc<dyn TokenBlacklistStore> {
    Arc::new(create_test_token_blacklist_store())
}

/// Creates a `UserService` with in-memory test dependencies.
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn create_test_user_service(pool: sqlx::PgPool) -> UserService {
    UserService::new_with_brute_force_service_and_runtime(
        &pool,
        UserServiceDependencies {
            jwt_service: create_test_jwt_service(),
            username_cache: UsernameCache::local_only("test:username:".to_string(), 128, 60),
            token_blacklist: create_test_token_blacklist_store_service(),
            key_builder: KeyBuilder::new("test"),
            brute_force: create_test_brute_force_protection_service(),
            password_complexity: synctv_core::config::PasswordComplexityConfig::default(),
        },
        UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..UserServiceRuntimeOptions::test_defaults()
        },
    )
}

/// Creates a `RoomService` with in-memory test dependencies where possible.
#[must_use]
pub fn create_test_room_service(pool: sqlx::PgPool) -> RoomService {
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    RoomService::new_with_options(
        pool.clone(),
        create_test_user_service(pool),
        RoomServiceOptions {
            runtime_settings_store: Some(Arc::new(RuntimeSettingsStore::new(settings_service))),
            ..RoomServiceOptions::test_defaults()
        },
    )
    .expect("room service should build")
}

/// Creates a request rate limiter trait object for testing.
#[must_use]
pub fn create_test_request_rate_limiter(prefix: &str) -> Arc<dyn RequestRateLimiterService> {
    Arc::new(RateLimiter::local_only(prefix.to_string()))
}
