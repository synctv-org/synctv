//! User auth/security service tests
//!
//! Tests for `UserService::refresh_token`, login status checks, `delete_user`,
//! `change_password/set_password`, and `create_or_load_by_oauth2`.
//!
//! S1/S2 tests use `InMemoryTokenBlacklistStore` + `InMemoryBruteForceProtection` + real `JwtService`.
//! S3/S7/S13 tests use testcontainers PG.
//!
use std::sync::Arc;

use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use sqlx::PgPool;
use std::collections::BTreeMap;
use synctv_common::ssrf::SsrfGuard;
use synctv_core::{
    cache::{CacheL2Backend, KeyBuilder, UsernameCache},
    models::{OAuth2Provider, SignupMethod, User, UserId},
    repository::{
        PasswordCredentialMaterial, SettingsRepository, UserEmailRepository,
        UserOAuthProviderRepository, UserPasswordRepository, UserRepository,
    },
    service::{
        local_oauth_state_store, AccountRegistrationOutcome, AuthFactorMethod, AuthenticatedLogin,
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, OAuth2GithubProviderConfig,
        OAuth2GoogleProviderConfig, OAuth2LinkResult, OAuth2ProviderConfig, OAuth2ProviderConfigs,
        OAuth2ProviderPrivateConfig, OAuth2Service, OAuth2ServiceRuntime, OpaquePasswordService,
        RateLimiter, RuntimeSettingsStore, SettingsService, TokenBlacklistStore,
        TokenCredentialBinding, UserService,
    },
    Error,
};
use synctv_core_testing::{
    create_test_pool, opaque_login_user, opaque_register_user, TestOptionExt, TestResultExt,
};

struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2_010::Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

async fn create_user_with_password_fixture(
    pool: &PgPool,
    username: String,
    email: Option<String>,
    password: &str,
) -> synctv_core::Result<User> {
    let normalized_username = username.trim().to_lowercase();
    let opaque_record = OpaquePasswordService::new_ephemeral_for_process().register_password(
        format!("synctv:user:{normalized_username}").as_bytes(),
        password,
    )?;
    let user = User::new(normalized_username, SignupMethod::Email);
    let mut tx = pool.begin().await?;
    let created = UserRepository::new(pool.clone())
        .create_with_executor(&user, &mut *tx)
        .await?;
    UserEmailRepository::new(pool.clone())
        .create_for_user_with_executor(&created, email.as_deref(), &mut *tx)
        .await?;
    UserPasswordRepository::new(pool.clone())
        .create_for_user_with_executor(
            &created,
            PasswordCredentialMaterial::opaque_only(&opaque_record),
            &mut *tx,
        )
        .await?;
    tx.commit().await?;
    Ok(created)
}

async fn insert_test_passkey(pool: &PgPool, user_id: &UserId, credential_id: &[u8]) {
    sqlx::query!(
        r"
        INSERT INTO auth_webauthn_credentials (
            user_id, credential_id, passkey, name
        )
        VALUES ($1, $2, '{}'::jsonb, 'test passkey')
        ",
        user_id.as_i64(),
        credential_id
    )
    .execute(pool)
    .await
    .checked("test passkey should be inserted");
}

const JWT_SECRET: &str = "test-secret-key-for-user-auth-service-tests-long-enough-1234567890";

fn create_jwt_service() -> JwtService {
    JwtService::with_durations(JWT_SECRET, 1, 30, 4, 60).checked("test operation should succeed")
}

fn create_user_service_with_components(
    pool: &PgPool,
    username_cache: UsernameCache,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    runtime: synctv_core::service::UserServiceRuntimeOptions,
) -> UserService {
    let jwt = create_jwt_service();
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_with_runtime(
        pool,
        jwt,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
        runtime,
    )
}

fn create_user_service_with_blacklist(
    pool: &PgPool,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
) -> UserService {
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    create_user_service_with_components(
        pool,
        username_cache,
        token_blacklist,
        default_test_user_runtime_options(),
    )
}

fn create_user_service(pool: &PgPool) -> UserService {
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    create_user_service_with_blacklist(pool, token_blacklist)
}

fn create_user_service_with_in_memory_blacklist(pool: &PgPool) -> UserService {
    create_user_service_with_blacklist(
        pool,
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400)),
    )
}

async fn register_password_user_refresh_token(service: &UserService, label: &str) -> String {
    let suffix = synctv_common::snanoid!(6);
    let username = format!("{label}_{suffix}");
    let email = Some(format!("{label}_{suffix}@test.com"));
    let (_user, Some(_access_token), Some(refresh_token)) =
        opaque_register_user(service, username.clone(), email, "StrongPass1")
            .await
            .checked("password registration should succeed")
    else {
        std::panic::panic_any("password registration should issue tokens");
    };

    refresh_token
}

async fn register_password_user_with_username(
    service: &UserService,
    label: &str,
) -> (User, String) {
    let suffix = synctv_common::snanoid!(6);
    let username = format!("{label}_{suffix}");
    let email = Some(format!("{label}_{suffix}@test.com"));
    let (user, _, _) = opaque_register_user(service, username.clone(), email, "StrongPass1")
        .await
        .checked("password registration should succeed");
    (user, username)
}

async fn run_concurrent_refresh_attempts(
    service: UserService,
    refresh_token: String,
    attempts: usize,
) -> usize {
    use tokio::sync::Barrier;

    let barrier = Arc::new(Barrier::new(attempts));
    let mut handles = Vec::with_capacity(attempts);

    for _ in 0..attempts {
        let service = service.clone();
        let token = refresh_token.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            service.refresh_token(token).await.is_ok()
        }));
    }

    let mut success_count = 0;
    for handle in handles {
        if handle.await.checked("refresh task should complete") {
            success_count += 1;
        }
    }

    success_count
}

fn default_test_user_runtime_options() -> synctv_core::service::UserServiceRuntimeOptions {
    synctv_core::service::UserServiceRuntimeOptions {
        password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
            enabled: true,
            need_review: false,
        }),
        ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
    }
}

fn oauth2_user_info(
    provider: OAuth2Provider,
    provider_instance_name: &str,
    provider_user_id: String,
    username: impl Into<String>,
) -> synctv_core::service::OAuth2UserInfo {
    synctv_core::service::OAuth2UserInfo {
        provider,
        provider_instance_name: provider_instance_name.to_string(),
        provider_issuer: None,
        provider_user_id,
        username: username.into(),
        avatar: None,
    }
}

async fn oauth2_service_with_google_signup(pool: &PgPool) -> OAuth2Service {
    oauth2_service_with_provider_signup(pool, "google", false).await
}

async fn oauth2_service_with_github_review(pool: &PgPool) -> OAuth2Service {
    oauth2_service_with_provider_signup(pool, "github", true).await
}

async fn oauth2_service_with_provider_signup(
    pool: &PgPool,
    provider_name: &str,
    signup_need_review: bool,
) -> OAuth2Service {
    let settings_service = Arc::new(SettingsService::new(
        SettingsRepository::new(pool.clone()),
        pool.clone(),
    ));
    let runtime_settings_store = Arc::new(RuntimeSettingsStore::new(settings_service));
    let provider_config = OAuth2ProviderConfig {
        enable_signup: true,
        signup_need_review,
        config: match provider_name {
            "github" => OAuth2ProviderPrivateConfig::GitHub(OAuth2GithubProviderConfig {
                client_id: format!("{provider_name}-client-id"),
                client_secret: format!("{provider_name}-client-secret"),
                redirect_url: "https://app.example.com/oauth2/callback".to_string(),
            }),
            "google" => OAuth2ProviderPrivateConfig::Google(OAuth2GoogleProviderConfig {
                client_id: format!("{provider_name}-client-id"),
                client_secret: format!("{provider_name}-client-secret"),
                redirect_url: "https://app.example.com/oauth2/callback".to_string(),
            }),
            other => panic!("unsupported test OAuth2 provider: {other}"),
        },
    };
    let oauth2_configs = OAuth2ProviderConfigs(BTreeMap::from([(
        provider_name.to_string(),
        provider_config,
    )]));
    let mut runtime_settings = runtime_settings_store
        .runtime_settings()
        .checked("runtime settings should load");
    runtime_settings.oauth2.providers = oauth2_configs;
    runtime_settings_store
        .persist_runtime_settings(&runtime_settings)
        .await
        .checked("OAuth2 runtime settings should persist");

    OAuth2Service::new_with_runtime(
        UserOAuthProviderRepository::new(pool.clone()),
        local_oauth_state_store(),
        synctv_core::oauth2::providers::provider_registry(SsrfGuard::strict_policy()),
        SsrfGuard::strict_policy(),
        false,
        OAuth2ServiceRuntime {
            runtime_settings_store: Some(runtime_settings_store),
            user_service: Some(Arc::new(create_user_service(pool))),
            ..OAuth2ServiceRuntime::default()
        },
    )
    .checked("OAuth2 service should initialize")
}

struct FamilyRevocationFailingStore {
    inner: InMemoryTokenBlacklistStore,
}

#[async_trait::async_trait]
impl TokenBlacklistStore for FamilyRevocationFailingStore {
    async fn is_blacklisted_checked(&self, key: &str) -> synctv_core::Result<bool> {
        self.inner.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<()> {
        self.inner.blacklist(key, ttl_secs).await
    }

    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<bool> {
        self.inner.blacklist_if_not_exists(key, ttl_secs).await
    }

    async fn get_family_revoked_at_checked(&self, key: &str) -> synctv_core::Result<Option<i64>> {
        self.inner.get_family_revoked_at_checked(key).await
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        Err(Error::Internal(
            "simulated family revocation persistence failure".to_string(),
        ))
    }
}

struct FamilyRevocationReadFailingStore {
    inner: InMemoryTokenBlacklistStore,
}

#[async_trait::async_trait]
impl TokenBlacklistStore for FamilyRevocationReadFailingStore {
    async fn is_blacklisted_checked(&self, key: &str) -> synctv_core::Result<bool> {
        self.inner.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<()> {
        self.inner.blacklist(key, ttl_secs).await
    }

    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<bool> {
        self.inner.blacklist_if_not_exists(key, ttl_secs).await
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Err(Error::Internal(
            "simulated family revocation read failure".to_string(),
        ))
    }

    async fn set_family_revoked(
        &self,
        key: &str,
        timestamp: i64,
        ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        self.inner
            .set_family_revoked(key, timestamp, ttl_secs)
            .await
    }
}

struct FailingCacheL2;

#[async_trait::async_trait]
impl CacheL2Backend for FailingCacheL2 {
    async fn get(&self, _key: &str) -> synctv_core::Result<Option<String>> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn set(&self, _key: &str, _json: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn delete(&self, _key: &str) -> synctv_core::Result<()> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn get_batch(&self, _keys: &[String]) -> synctv_core::Result<Vec<Option<String>>> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn set_if_newer(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _new_ts_millis: i64,
    ) -> synctv_core::Result<bool> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn set_if_version_at_least(
        &self,
        _key: &str,
        _json: &str,
        _ttl_secs: u64,
        _version: i64,
    ) -> synctv_core::Result<bool> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    async fn delete_by_prefix(&self, _prefix: &str) -> synctv_core::Result<()> {
        Err(Error::Internal(
            "simulated username cache backend failure".to_string(),
        ))
    }

    fn is_active(&self) -> bool {
        true
    }
}

fn create_user_service_with_failing_username_cache(pool: &PgPool) -> UserService {
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let username_cache = UsernameCache::new(
        Arc::new(FailingCacheL2),
        "test:username:".to_string(),
        1000,
        60,
    );
    create_user_service_with_components(
        pool,
        username_cache,
        token_blacklist,
        default_test_user_runtime_options(),
    )
}

fn opaque_client_registration_start(
    rng: &mut OsRng,
    password: &[u8],
    context: &str,
) -> opaque_ke::ClientRegistrationStartResult<TestOpaqueCipherSuite> {
    match ClientRegistration::<TestOpaqueCipherSuite>::start(rng, password) {
        Ok(start) => start,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

fn opaque_client_login_start(
    rng: &mut OsRng,
    password: &[u8],
    context: &str,
) -> opaque_ke::ClientLoginStartResult<TestOpaqueCipherSuite> {
    match ClientLogin::<TestOpaqueCipherSuite>::start(rng, password) {
        Ok(start) => start,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

fn checked_opaque_registration_finish(
    result: Result<
        opaque_ke::ClientRegistrationFinishResult<TestOpaqueCipherSuite>,
        opaque_ke::errors::ProtocolError,
    >,
    context: &str,
) -> opaque_ke::ClientRegistrationFinishResult<TestOpaqueCipherSuite> {
    match result {
        Ok(finish) => finish,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

fn checked_opaque_login_finish(
    result: Result<
        opaque_ke::ClientLoginFinishResult<TestOpaqueCipherSuite>,
        opaque_ke::errors::ProtocolError,
    >,
    context: &str,
) -> opaque_ke::ClientLoginFinishResult<TestOpaqueCipherSuite> {
    match result {
        Ok(finish) => finish,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

async fn opaque_register(
    service: &UserService,
    username: String,
    email: Option<String>,
    password: &str,
) -> synctv_core::Result<(synctv_core::models::User, Option<String>, Option<String>)> {
    let mut rng = OsRng;
    let client_start = opaque_client_registration_start(
        &mut rng,
        password.as_bytes(),
        "client OPAQUE registration start should succeed",
    );
    let challenge = service
        .start_opaque_registration_with_control(
            username,
            email,
            client_start.message.serialize().to_vec().into(),
            None,
            None,
        )
        .await?;
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .checked("server registration response should deserialize");
    let client_finish = checked_opaque_registration_finish(
        client_start.state.finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        ),
        "client OPAQUE registration finish should succeed",
    );

    match service
        .finish_opaque_registration_with_control(
            &challenge.session_id,
            client_finish.message.serialize().to_vec().into(),
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
        Ok(AccountRegistrationOutcome::PendingReview(_)) => Err(Error::Internal(
            "opaque_register helper received pending review outcome".to_string(),
        )),
        Err(error) => Err(error),
    }
}

async fn opaque_update_password(
    service: &UserService,
    user_id: &UserId,
    current_password: &str,
    new_password: &str,
) -> synctv_core::Result<synctv_core::models::User> {
    let mut rng = OsRng;
    let login_start = opaque_client_login_start(
        &mut rng,
        current_password.as_bytes(),
        "client OPAQUE login start should succeed",
    );
    let registration_start = opaque_client_registration_start(
        &mut rng,
        new_password.as_bytes(),
        "client OPAQUE registration start should succeed",
    );
    let challenge = service
        .start_opaque_password_update(
            user_id,
            login_start.message.serialize().to_vec().into(),
            registration_start.message.serialize().to_vec().into(),
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .checked("server credential response should deserialize");
    let login_finish = checked_opaque_login_finish(
        login_start.state.finish(
            &mut rng,
            current_password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        ),
        "client OPAQUE login finish should succeed",
    );
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .checked("server registration response should deserialize");
    let registration_finish = checked_opaque_registration_finish(
        registration_start.state.finish(
            &mut rng,
            new_password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        ),
        "client OPAQUE registration finish should succeed",
    );

    service
        .finish_opaque_password_update(
            user_id,
            &challenge.session_id,
            login_finish.message.serialize().to_vec().into(),
            registration_finish.message.serialize().to_vec().into(),
        )
        .await
}

async fn opaque_reset_password_after_external_verification(
    service: &UserService,
    user_id: &UserId,
    new_password: &str,
) -> synctv_core::Result<synctv_core::models::User> {
    let mut rng = OsRng;
    let registration_start = opaque_client_registration_start(
        &mut rng,
        new_password.as_bytes(),
        "client OPAQUE registration start should succeed",
    );
    let challenge = service
        .start_opaque_password_reset_after_external_verification(
            user_id,
            registration_start.message.serialize().to_vec().into(),
        )
        .await?;
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .checked("server registration response should deserialize");
    let registration_finish = checked_opaque_registration_finish(
        registration_start.state.finish(
            &mut rng,
            new_password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        ),
        "client OPAQUE registration finish should succeed",
    );

    service
        .finish_opaque_password_reset_after_external_verification(
            &challenge.session_id,
            registration_finish.message.serialize().to_vec().into(),
        )
        .await
}

async fn pending_passkey_opaque_update_upload(
    service: &UserService,
    user_id: &UserId,
    new_password: &str,
) -> synctv_core::Result<(String, Vec<u8>)> {
    let mut rng = OsRng;
    let registration_start = opaque_client_registration_start(
        &mut rng,
        new_password.as_bytes(),
        "client OPAQUE registration start should succeed",
    );
    let challenge = service
        .start_opaque_password_update_pending_passkey_verification(
            user_id,
            registration_start.message.serialize().to_vec().into(),
        )
        .await?;
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .checked("server registration response should deserialize");
    let registration_finish = checked_opaque_registration_finish(
        registration_start.state.finish(
            &mut rng,
            new_password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        ),
        "client OPAQUE registration finish should succeed",
    );

    Ok((
        challenge.session_id,
        registration_finish.message.serialize().to_vec(),
    ))
}

async fn opaque_login(
    service: &UserService,
    identifier: String,
    password: &str,
) -> synctv_core::Result<(synctv_core::models::User, String, String)> {
    let login = opaque_login_outcome(service, identifier, password).await?;
    match login {
        AuthenticatedLogin::Complete {
            user,
            email: _,
            access_token,
            refresh_token,
        } => Ok((user, access_token, refresh_token)),
        AuthenticatedLogin::MfaRequired { .. } => Err(Error::Authentication(
            "Unexpected MFA challenge in opaque_login test helper".to_string(),
        )),
    }
}

async fn opaque_login_outcome(
    service: &UserService,
    identifier: String,
    password: &str,
) -> synctv_core::Result<AuthenticatedLogin> {
    let login_session = service
        .start_login_with_control(identifier, true, true, None, None)
        .await?;
    let mut rng = OsRng;
    let client_start = opaque_client_login_start(
        &mut rng,
        password.as_bytes(),
        "client OPAQUE login start should succeed",
    );
    let challenge = service
        .start_opaque_login_with_control(
            &login_session.session_id,
            client_start.message.serialize().to_vec().into(),
            None,
            None,
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .checked("server credential response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

    let login = service
        .finish_opaque_login_with_control(
            &challenge.session_id,
            client_finish.message.serialize().to_vec().into(),
            None,
            None,
        )
        .await?;
    Ok(login)
}

#[tokio::test]
async fn test_start_login_reports_account_primary_methods() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let suffix = synctv_common::snanoid!(8);
    let username = format!("login_methods_{suffix}");
    let email = format!("login_methods_{suffix}@test.com");
    let user = create_user_with_password_fixture(
        &pool,
        username.clone(),
        Some(email.clone()),
        "StrongPass1",
    )
    .await
    .checked("password user should be created");
    insert_test_passkey(&pool, &user.id, format!("credential-{suffix}").as_bytes()).await;

    let challenge = service
        .start_login_with_control(username, true, true, None, None)
        .await
        .checked("login session should start");
    assert_eq!(
        challenge.available_methods,
        vec![
            AuthFactorMethod::WebAuthn,
            AuthFactorMethod::Password,
            AuthFactorMethod::Email,
        ]
    );
    let session = service
        .get_login_session_for_method(&challenge.session_id, AuthFactorMethod::Email)
        .await
        .checked("email method should be available");
    assert_eq!(session.user_id(), Some(user.id));
    assert_eq!(session.email(), Some(email.as_str()));

    let password_only = service
        .start_login_with_control(email, false, false, None, None)
        .await
        .checked("server capability filters should be applied");
    assert_eq!(
        password_only.available_methods,
        vec![AuthFactorMethod::Password]
    );

    let passkey_only_username = format!("passkey_only_{suffix}");
    let passkey_only_user = UserRepository::new(pool.clone())
        .create(&User::new(
            passkey_only_username.clone(),
            SignupMethod::WebAuthn,
        ))
        .await
        .checked("passkey-only user should be created");
    insert_test_passkey(
        &pool,
        &passkey_only_user.id,
        format!("passkey-only-credential-{suffix}").as_bytes(),
    )
    .await;
    let passkey_only = service
        .start_login_with_control(passkey_only_username, true, true, None, None)
        .await
        .checked("passkey-only login session should start");
    assert_eq!(
        passkey_only.available_methods,
        vec![AuthFactorMethod::WebAuthn]
    );

    let email_only_username = format!("email_only_{suffix}");
    let email_only_address = format!("email_only_{suffix}@test.com");
    let email_only_user = UserRepository::new(pool.clone())
        .create(&User::new(email_only_username.clone(), SignupMethod::Email))
        .await
        .checked("email-only user should be created");
    UserEmailRepository::new(pool.clone())
        .create_for_user_with_executor(&email_only_user, Some(&email_only_address), &pool)
        .await
        .checked("email-only identity should be created");
    let email_only = service
        .start_login_with_control(email_only_username, true, true, None, None)
        .await
        .checked("email-only login session should start");
    assert_eq!(email_only.available_methods, vec![AuthFactorMethod::Email]);

    let unknown_identifier = format!("missing_{suffix}@test.com");
    let unknown = service
        .start_login_with_control(unknown_identifier.clone(), true, true, None, None)
        .await
        .checked("unknown identifiers should receive a decoy login session");
    let repeated_unknown = service
        .start_login_with_control(unknown_identifier, true, true, None, None)
        .await
        .checked("unknown identifier profile should be repeatable");
    assert_eq!(
        unknown.available_methods,
        repeated_unknown.available_methods
    );
    assert!(!unknown.available_methods.is_empty());
    assert!(unknown.available_methods.iter().all(|method| matches!(
        method,
        AuthFactorMethod::Password | AuthFactorMethod::WebAuthn | AuthFactorMethod::Email
    )));
}

#[tokio::test]
async fn test_login_session_has_one_atomic_confirmation_claim() {
    let (_container, pool) = create_test_pool().await;
    let service = Arc::new(create_user_service(&pool));
    let suffix = synctv_common::snanoid!(8);
    let username = format!("email_claim_{suffix}");
    let email = format!("email_claim_{suffix}@test.com");
    let user = UserRepository::new(pool.clone())
        .create(&User::new(username.clone(), SignupMethod::Email))
        .await
        .checked("email login user should be created");
    UserEmailRepository::new(pool.clone())
        .create_for_user_with_executor(&user, Some(&email), &pool)
        .await
        .checked("email identity should be created");
    let login = service
        .start_login_with_control(username, true, true, None, None)
        .await
        .checked("login session should start");

    let first_service = Arc::clone(&service);
    let first_session_id = login.session_id.clone();
    let second_service = Arc::clone(&service);
    let second_session_id = login.session_id;
    let (first, second) = tokio::join!(
        first_service.consume_login_session_for_method(&first_session_id, AuthFactorMethod::Email,),
        second_service
            .consume_login_session_for_method(&second_session_id, AuthFactorMethod::Email,),
    );

    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
}

#[tokio::test]
async fn test_opaque_challenge_keeps_identified_login_session_reusable() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let suffix = synctv_common::snanoid!(8);
    let username = format!("opaque_retry_{suffix}");
    create_user_with_password_fixture(&pool, username.clone(), None, "StrongPass1")
        .await
        .checked("password user should be created");

    let login = service
        .start_login_with_control(username, true, true, None, None)
        .await
        .checked("login session should start");
    let mut rng = OsRng;
    let client_start = opaque_client_login_start(
        &mut rng,
        b"WrongPass1",
        "client OPAQUE login start should succeed",
    );
    let challenge = service
        .start_opaque_login_with_control(
            &login.session_id,
            client_start.message.serialize().to_vec().into(),
            None,
            None,
        )
        .await
        .checked("OPAQUE challenge should start");

    assert_ne!(challenge.session_id, login.session_id);
    service
        .get_login_session_for_method(&login.session_id, AuthFactorMethod::Password)
        .await
        .checked("identified session should remain available for another attempt");
}

fn expect_complete_login(login: AuthenticatedLogin) -> (synctv_core::models::User, String, String) {
    match login {
        AuthenticatedLogin::Complete {
            user,
            email: _,
            access_token,
            refresh_token,
        } => (user, access_token, refresh_token),
        AuthenticatedLogin::MfaRequired { .. } => {
            std::panic::panic_any("expected complete login, got MFA challenge")
        }
    }
}

struct PasswordCredentialRow {
    opaque_record: Option<Vec<u8>>,
    opaque_credential_identifier: Option<Vec<u8>>,
    version: i32,
}

async fn load_password_credential_row(pool: &PgPool, user_id: UserId) -> PasswordCredentialRow {
    let row = sqlx::query!(
        r#"
        SELECT opaque_record, opaque_credential_identifier, opaque_ciphersuite,
               opaque_server_setup_version, version AS "version!"
        FROM auth_password_credentials
        WHERE user_id = $1
        "#,
        user_id.as_i64()
    )
    .fetch_one(pool)
    .await
    .checked("password credential row should exist");
    PasswordCredentialRow {
        opaque_record: row.opaque_record,
        opaque_credential_identifier: row.opaque_credential_identifier,
        version: row.version,
    }
}

// S1: UserService::refresh_token (Refresh Token Rotation)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_happy_path() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, Some(access_token), Some(refresh_token)) = opaque_register_user(
        &service,
        format!("refresh_user_{}", synctv_common::snanoid!(6)),
        Some(format!("refresh_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens from registration");
    };

    let (new_access, new_refresh) = service
        .refresh_token(refresh_token.clone())
        .await
        .checked("Refresh should succeed");

    let jwt = create_jwt_service();
    let access_claims = jwt
        .verify_access_token(&new_access)
        .checked("New access token valid");
    let refresh_claims = jwt
        .verify_refresh_token(&new_refresh)
        .checked("New refresh token valid");

    assert_eq!(access_claims.sub, user.id.to_string());
    assert_eq!(refresh_claims.sub, user.id.to_string());

    assert_ne!(new_access, access_token);
    assert_ne!(new_refresh, refresh_token);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_old_jti_blacklisted_before_new_issued() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(&pool, token_blacklist.clone());

    let (_user, _access, Some(refresh_token)) = opaque_register_user(
        &service,
        format!("blacklist_user_{}", synctv_common::snanoid!(6)),
        Some(format!("blacklist_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    let jwt = create_jwt_service();
    let old_claims = jwt
        .verify_refresh_token(&refresh_token)
        .checked("Old refresh token valid");
    let old_jti = old_claims.jti.clone();

    let _new_tokens = service
        .refresh_token(refresh_token.clone())
        .await
        .checked("Refresh should succeed");

    let key_builder = KeyBuilder::new("test");
    let blacklist_key = key_builder.refresh_token_blacklist(&old_jti);
    assert!(
        token_blacklist
            .is_blacklisted_checked(&blacklist_key)
            .await
            .checked("test operation should succeed"),
        "Old JTI should be blacklisted after refresh"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_replay_same_jti_triggers_family_revocation() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(&pool, token_blacklist.clone());

    let (_user, _access, Some(refresh_token)) = opaque_register_user(
        &service,
        format!("replay_user_{}", synctv_common::snanoid!(6)),
        Some(format!("replay_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    let (_new_access, new_refresh) = service
        .refresh_token(refresh_token.clone())
        .await
        .checked("First refresh should succeed");

    let replay_result = service.refresh_token(refresh_token.clone()).await;
    assert!(
        replay_result.is_err(),
        "Replayed refresh token should be rejected"
    );
    assert!(matches!(
        replay_result.failed("operation should fail"),
        Error::Authentication(_)
    ));

    let second_refresh = service.refresh_token(new_refresh).await;
    assert!(
        second_refresh.is_err(),
        "New refresh token should also be rejected after family revocation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_logout_session_revocation_blocks_only_current_refresh_session() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(&pool, token_blacklist);

    let username = format!("logout_session_{}", synctv_common::snanoid!(6));
    let email = Some(format!(
        "logout_session_{}@test.com",
        synctv_common::snanoid!(6)
    ));
    let (user, Some(first_access), Some(first_refresh)) =
        opaque_register_user(&service, username.clone(), email, "StrongPass1")
            .await
            .checked("Registration should succeed")
    else {
        std::panic::panic_any("expected tokens");
    };
    let AuthenticatedLogin::Complete {
        refresh_token: second_refresh,
        ..
    } = opaque_login_user(&service, username, "StrongPass1")
        .await
        .checked("Second login should succeed")
    else {
        std::panic::panic_any("expected complete login");
    };

    let jwt = create_jwt_service();
    let access_claims = jwt
        .verify_access_token(&first_access)
        .checked("Access token should be valid");
    let revoked_at = access_claims.iat.saturating_add(1);

    service
        .revoke_refresh_token_session(
            &user.id,
            access_claims
                .sid
                .as_deref()
                .checked("access token should carry sid"),
            revoked_at,
        )
        .await
        .checked("Logout session revocation should persist");

    let first_result = service.refresh_token(first_refresh).await;
    assert!(
        first_result.is_err(),
        "Refresh token from logged-out session should be rejected"
    );

    let second_result = service.refresh_token(second_refresh).await;
    assert!(
        second_result.is_ok(),
        "Another login session should not be revoked by current-session logout"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rejects_invalid_user_state_and_password_version() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let repo = UserRepository::new(pool.clone());

    let (_user, _access, Some(refresh_token)) = opaque_register_user(
        &service,
        format!("pv_user_{}", synctv_common::snanoid!(6)),
        Some(format!("pv_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    let jwt = create_jwt_service();
    let claims = jwt
        .verify_refresh_token(&refresh_token)
        .checked("Token valid");
    let user_id = claims
        .sub
        .parse::<UserId>()
        .checked("valid numeric user id claim");
    opaque_update_password(&service, &user_id, "StrongPass1", "NewStrongPass1")
        .await
        .checked("Password change should succeed");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh with old password version should be rejected"
    );
    let error = result.failed("operation should fail");
    assert!(
        matches!(error, Error::Authentication(_)),
        "expected Authentication, got {error:?}"
    );

    let (strict_user, _access, Some(_refresh_token)) = opaque_register_user(
        &service,
        format!("pv_strict_{}", synctv_common::snanoid!(6)),
        Some(format!("pv_strict_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    let jwt = create_jwt_service();
    let mismatched_refresh = jwt
        .sign_refresh_token_with_session(
            &strict_user.id,
            99,
            None,
            "strict-password-version-session",
            &TokenCredentialBinding::Password { version: 99 },
        )
        .checked("mismatched refresh token should be signed");

    let result = service.refresh_token(mismatched_refresh).await;
    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "Refresh token with any mismatched password version should be rejected"
    );

    let (banned_user, _access, Some(banned_refresh_token)) = opaque_register_user(
        &service,
        format!("banned_refresh_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "banned_refresh_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    repo.ban(&banned_user.id, None, Some("test ban".to_string()))
        .await
        .checked("Failed to ban user");

    let result = service.refresh_token(banned_refresh_token).await;
    assert!(result.is_err(), "Banned user should not be able to refresh");
    let error = result.failed("operation should fail");
    assert!(
        matches!(error, Error::Authentication(_)),
        "expected Authentication, got {error:?}"
    );

    let (deleted_user, _access, Some(deleted_refresh_token)) = opaque_register_user(
        &service,
        format!("deleted_refresh_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "deleted_refresh_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    sqlx::query!(
        "UPDATE users SET deleted_at = NOW() WHERE id = $1",
        deleted_user.id.as_i64()
    )
    .execute(&pool)
    .await
    .checked("Failed to soft-delete");

    let result = service.refresh_token(deleted_refresh_token).await;
    assert!(
        result.is_err(),
        "Deleted user should not be able to refresh"
    );
    let error = result.failed("operation should fail");
    assert!(
        matches!(error, Error::Authentication(_)),
        "expected Authentication, got {error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_family_revocation_timestamp_blocks_older_tokens() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(&pool, token_blacklist.clone());

    let (_user, _access, Some(refresh_token_1)) = opaque_register_user(
        &service,
        format!("family_rev_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "family_rev_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected tokens");
    };

    let (_access_2, refresh_token_2) = service
        .refresh_token(refresh_token_1.clone())
        .await
        .checked("First refresh should succeed");

    let (_access_3, refresh_token_3) = service
        .refresh_token(refresh_token_2.clone())
        .await
        .checked("Second refresh should succeed");

    let replay_result = service.refresh_token(refresh_token_1).await;
    assert!(replay_result.is_err(), "Replayed old token should fail");

    let result = service.refresh_token(refresh_token_3).await;
    assert!(
        result.is_err(),
        "Token issued before family revocation should be blocked"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_fails_closed_when_family_revocation_lookup_errors() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(FamilyRevocationReadFailingStore {
            inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        });
    let service = create_user_service_with_blacklist(&pool, token_blacklist);

    let (_user, _access, Some(refresh_token)) = opaque_register_user(
        &service,
        format!("family_lookup_fail_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "family_lookup_fail_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed") else {
        std::panic::panic_any("expected refresh token");
    };

    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh should fail closed when family revocation lookup cannot be verified"
    );
    assert!(matches!(
        result.failed("operation should fail"),
        Error::Internal(_)
    ));
}

// S2: UserService::login status checks

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_registration_persists_login_credential() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let username = format!("plain_password_default_{}", synctv_common::snanoid!(6));
    opaque_register_user(
        &service,
        username.clone(),
        Some(format!(
            "plain_password_default_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("OPAQUE registration should create a user");

    let created = service
        .get_user_by_username(&username)
        .await
        .checked("registered user should be fetchable");
    let row = load_password_credential_row(&pool, created.id).await;
    assert!(
        row.opaque_record.is_some() && row.opaque_credential_identifier.is_some(),
        "OPAQUE registration must persist OPAQUE credential material"
    );

    let opaque_result = opaque_login_user(&service, username, "StrongPass1").await;
    assert!(
        opaque_result.is_ok(),
        "OPAQUE login must use the persisted password credential"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_rejects_inactive_accounts() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let repo = UserRepository::new(pool.clone());

    let username = format!("banned_login_{}", synctv_common::snanoid!(6));
    let (user, _, _) = opaque_register_user(
        &service,
        username.clone(),
        Some(format!(
            "banned_login_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    repo.ban(&user.id, None, Some("test ban".to_string()))
        .await
        .checked("Failed to ban user");

    let result = opaque_login_user(&service, username, "StrongPass1").await;
    assert!(result.is_err(), "Banned user should not be able to login");
    assert!(matches!(
        result.failed("operation should fail"),
        Error::Authentication(_)
    ));

    let rejected_username = format!("rejected_login_{}", synctv_common::snanoid!(6));
    let (rejected_user, _, _) = opaque_register_user(
        &service,
        rejected_username.clone(),
        Some(format!(
            "rejected_login_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    sqlx::query!(
        r"
        INSERT INTO user_registration_requests (
            id, username, email, opaque_record,
            opaque_credential_identifier, opaque_ciphersuite,
            opaque_server_setup_version, signup_method, status,
            requested_at, reviewed_at, rejection_reason
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, $10)
        ",
        synctv_core::models::generate_id(),
        &format!("rejected_request_{}", synctv_common::snanoid!(6)),
        Option::<String>::None,
        b"not-used-opaque-record".as_slice(),
        b"not-used-opaque-id".as_slice(),
        "opaque-ristretto255-sha512-argon2id",
        1_i32,
        i16::from(rejected_user.signup_method),
        i16::from(synctv_core::models::ReviewStatus::Rejected),
        "rejected by test"
    )
    .execute(&pool)
    .await
    .checked("Failed to create rejected registration request");

    repo.ban(
        &rejected_user.id,
        None,
        Some("rejected account cannot login".to_string()),
    )
    .await
    .checked("Failed to disable rejected test user");

    let result = opaque_login_user(&service, rejected_username, "StrongPass1").await;
    assert!(result.is_err(), "Rejected user should not be able to login");
    assert!(matches!(
        result.failed("operation should fail"),
        Error::Authentication(_)
    ));

    let deleted_username = format!("deleted_login_{}", synctv_common::snanoid!(6));
    let (deleted_user, _, _) = opaque_register_user(
        &service,
        deleted_username.clone(),
        Some(format!(
            "deleted_login_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    sqlx::query!(
        "UPDATE users SET deleted_at = NOW() WHERE id = $1",
        deleted_user.id.as_i64()
    )
    .execute(&pool)
    .await
    .checked("Failed to soft-delete");

    let result = opaque_login_user(&service, deleted_username, "StrongPass1").await;
    assert!(
        result.is_err(),
        "Soft-deleted user should not be able to login"
    );
    let error = result.failed("operation should fail");
    assert!(
        matches!(error, Error::Authentication(_)),
        "expected Authentication, got {error:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_email_and_oauth2_user_types_allowed() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let email_user = format!("email_allowed_{}", synctv_common::snanoid!(6));
    let (_user_with_email, _, _) = opaque_register_user(
        &service,
        email_user.clone(),
        Some(format!("{email_user}@test.com")),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;
    let oauth_user = service
        .create_or_load_by_oauth2(&provider, "oauth_allowed", "oauth_allowed")
        .await
        .checked("OAuth2 user creation should succeed");

    opaque_reset_password_after_external_verification(&service, &oauth_user.id, "StrongPass1")
        .await
        .checked("Setting password through OPAQUE reset should succeed");

    let email_result = opaque_login_user(&service, email_user, "StrongPass1").await;

    let oauth_result = opaque_login(&service, oauth_user.username.clone(), "StrongPass1").await;

    assert!(
        email_result.is_ok(),
        "Email user should be allowed when verification not required: {:?}",
        email_result.err()
    );
    assert!(
        oauth_result.is_ok(),
        "OAuth2 user should be allowed when verification not required: {:?}",
        oauth_result.err()
    );
}

// S3: UserService::delete_user

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_already_deleted_guard() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, _, _) = opaque_register_user(
        &service,
        format!("del_guard_{}", synctv_common::snanoid!(6)),
        Some(format!("del_guard_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    service
        .delete_user(&user.id)
        .await
        .checked("First delete should succeed");

    let result = service.delete_user(&user.id).await;
    assert!(result.is_err(), "Double delete should fail");
    let err = result.failed("operation should fail");
    match &err {
        Error::InvalidInput(msg) => assert!(
            msg.contains("already deleted"),
            "Expected 'already deleted' message, got: {msg}"
        ),
        Error::NotFound(_) => {}
        _ => std::panic::panic_any(format!("expected InvalidInput or NotFound, got: {err}")),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_transaction_atomicity_with_oauth2() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, _, _) = opaque_register_user(
        &service,
        format!("del_oauth_{}", synctv_common::snanoid!(6)),
        Some(format!("del_oauth_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    service
        .delete_user(&user.id)
        .await
        .checked("Delete with OAuth2 cleanup should succeed");

    let deleted_user: Option<i64> = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM users WHERE id = $1 AND deleted_at IS NOT NULL"#,
        user.id.as_i64()
    )
    .fetch_optional(&pool)
    .await
    .checked("Query should succeed");
    assert!(
        deleted_user.is_some(),
        "User should be soft-deleted in the database"
    );
}

// S7: forced password reset

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_force_password_reset_revokes_password_credential_and_bumps_version() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, _, _) = opaque_register_user(
        &service,
        format!("setpw_{}", synctv_common::snanoid!(6)),
        Some(format!("setpw_{}@test.com", synctv_common::snanoid!(6))),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    let old_version = load_password_credential_row(&pool, user.id).await.version;

    let _updated_user = service
        .force_password_reset(&user.id)
        .await
        .checked("forced password reset should succeed");
    let after_version = load_password_credential_row(&pool, user.id).await.version;

    assert_eq!(
        after_version,
        old_version + 1,
        "Password version should be incremented by forced reset"
    );

    let row = load_password_credential_row(&pool, user.id).await;
    assert!(
        row.opaque_record.is_none(),
        "forced password reset must revoke OPAQUE credential material"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_registration_creates_opaque_only_password_credential() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let username = format!("opaque_reg_{}", synctv_common::snanoid!(6));
    let (user, access_token, refresh_token) = opaque_register(
        &service,
        username.clone(),
        Some(format!(
            "opaque_reg_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("OPAQUE registration should succeed");

    assert!(
        access_token.is_some() && refresh_token.is_some(),
        "OPAQUE registration should issue tokens"
    );

    let row = load_password_credential_row(&pool, user.id).await;
    assert!(
        row.opaque_record.is_some() && row.opaque_credential_identifier.is_some(),
        "OPAQUE-specific registration must persist OPAQUE credential material"
    );
    assert!(
        service
            .has_usable_password_authentication(&user)
            .await
            .checked("password auth capability check should succeed"),
        "OPAQUE-only registration must count as usable password authentication"
    );

    let password_login = opaque_login_user(&service, username, "StrongPass1").await;
    assert!(
        password_login.is_ok(),
        "OPAQUE login should work for OPAQUE-only registrations"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_only_password_counts_as_first_factor_without_plaintext_mfa() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let username = format!("opaque_mfa_{}", synctv_common::snanoid!(6));
    let email = format!("opaque_mfa_{}@test.com", synctv_common::snanoid!(6));
    let (user, _, _) = opaque_register_user(&service, username.clone(), Some(email), "StrongPass1")
        .await
        .checked("user creation should succeed");

    let (_preferences, factors) = service
        .get_user_preferences(&user.id)
        .await
        .checked("preferences should load");
    assert!(factors.password);
    assert!(factors.email);
    assert!(factors.supports_two_factor());

    let result = service.set_two_factor_enabled(&user.id, true).await;
    assert!(
        result.is_ok(),
        "OPAQUE password plus email should allow enabling 2FA"
    );

    let login = opaque_login_outcome(&service, username, "StrongPass1")
        .await
        .checked("OPAQUE password first factor should start MFA");
    let AuthenticatedLogin::MfaRequired { challenge, .. } = login else {
        std::panic::panic_any("2FA-enabled OPAQUE password login should require a second factor");
    };
    assert!(
        challenge
            .available_methods
            .contains(&AuthFactorMethod::Email),
        "email should be available after an OPAQUE password first factor"
    );
    assert!(
        !challenge
            .available_methods
            .contains(&AuthFactorMethod::Password),
        "password must not be offered as a plaintext MFA completion method"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_password_update_replaces_opaque_password_credential() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, username) = register_password_user_with_username(&service, "opaque_update").await;

    let before = load_password_credential_row(&pool, user.id).await;
    assert!(
        before.opaque_record.is_some(),
        "password registration should store OPAQUE credential material"
    );
    let before_version = before.version;

    let updated_user = opaque_update_password(&service, &user.id, "StrongPass1", "NewStrongPass1")
        .await
        .checked("OPAQUE password update should succeed");
    let after = load_password_credential_row(&pool, user.id).await;
    let after_version = after.version;
    assert_eq!(
        after_version,
        before_version + 1,
        "OPAQUE password update must invalidate existing tokens by bumping version"
    );

    assert!(
        after.opaque_record.is_some(),
        "OPAQUE password update must persist the new OPAQUE credential"
    );
    assert!(
        service
            .has_usable_password_authentication(&updated_user)
            .await
            .checked("password auth capability check should succeed"),
        "OPAQUE-only password update must count as usable password authentication"
    );

    let old_password_login = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    let new_password_login = opaque_login_user(&service, username.clone(), "NewStrongPass1").await;
    assert!(
        old_password_login.is_err() && new_password_login.is_ok(),
        "OPAQUE login should use the updated password credential"
    );

    let opaque_login_result = opaque_login_user(&service, username, "NewStrongPass1").await;
    assert!(
        opaque_login_result.is_ok(),
        "OPAQUE login must work with the updated OPAQUE-only password credential"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_password_reset_replaces_opaque_password_credential() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, username) = register_password_user_with_username(&service, "opaque_reset").await;

    let before = load_password_credential_row(&pool, user.id).await;
    assert!(
        before.opaque_record.is_some(),
        "password registration should store OPAQUE credential material"
    );
    let before_version = before.version;

    let _updated_user =
        opaque_reset_password_after_external_verification(&service, &user.id, "NewStrongPass1")
            .await
            .checked("OPAQUE password reset should succeed");
    let after = load_password_credential_row(&pool, user.id).await;
    let after_version = after.version;
    assert_eq!(
        after_version,
        before_version + 1,
        "OPAQUE password reset must invalidate existing tokens by bumping version"
    );

    assert!(
        after.opaque_record.is_some(),
        "OPAQUE password reset must persist the new OPAQUE credential"
    );

    let old_password_login = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    let new_password_login = opaque_login_user(&service, username.clone(), "NewStrongPass1").await;
    assert!(
        old_password_login.is_err() && new_password_login.is_ok(),
        "OPAQUE login should use the reset password credential"
    );

    let opaque_login_result = opaque_login_user(&service, username, "NewStrongPass1").await;
    assert!(
        opaque_login_result.is_ok(),
        "OPAQUE login must work with the reset OPAQUE-only password credential"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_password_update_requires_current_credential_proof() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, username) =
        register_password_user_with_username(&service, "opaque_update_proof").await;

    let mut rng = OsRng;
    let login_start = opaque_client_login_start(
        &mut rng,
        b"WrongStrongPass1",
        "client OPAQUE login start should succeed",
    );
    let registration_start = opaque_client_registration_start(
        &mut rng,
        b"NewStrongPass1",
        "client OPAQUE registration start should succeed",
    );
    let challenge = service
        .start_opaque_password_update(
            &user.id,
            login_start.message.serialize().to_vec().into(),
            registration_start.message.serialize().to_vec().into(),
        )
        .await
        .checked("starting an OPAQUE password update should not prove the password yet");
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .checked("server registration response should deserialize");
    let registration_finish = checked_opaque_registration_finish(
        registration_start.state.finish(
            &mut rng,
            b"NewStrongPass1",
            registration_response,
            ClientRegistrationFinishParameters::default(),
        ),
        "client OPAQUE registration finish should succeed",
    );

    let result = service
        .finish_opaque_password_update(
            &user.id,
            &challenge.session_id,
            b"invalid-current-credential-proof".to_vec().into(),
            registration_finish.message.serialize().to_vec().into(),
        )
        .await;
    assert!(
        result.is_err(),
        "OPAQUE password update must reject requests that cannot prove the current credential"
    );

    let old_password_login = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    assert!(
        old_password_login.is_ok(),
        "failed OPAQUE password update must not replace the existing password credential"
    );

    let new_opaque_login = opaque_login(&service, username, "NewStrongPass1").await;
    assert!(
        new_opaque_login.is_err(),
        "failed OPAQUE password update must not install the requested new credential"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_password_update_requires_passkey_finish_for_pending_passkey_session() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, username) =
        register_password_user_with_username(&service, "opaque_passkey_update").await;

    let (session_id, registration_upload) =
        pending_passkey_opaque_update_upload(&service, &user.id, "NewStrongPass1")
            .await
            .checked("pending passkey OPAQUE update should start");

    let bypass_result = service
        .finish_opaque_password_update_after_external_verification(
            &user.id,
            &session_id,
            registration_upload.into(),
        )
        .await;
    assert!(
        matches!(bypass_result, Err(Error::Authentication(_))),
        "pending passkey sessions must not be finishable through generic external verification"
    );

    let password_login = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    assert!(
        password_login.is_ok(),
        "failed passkey-bypass attempt must leave the original password intact"
    );

    let (session_id, registration_upload) =
        pending_passkey_opaque_update_upload(&service, &user.id, "NewStrongPass1")
            .await
            .checked("second pending passkey OPAQUE update should start");
    let updated_user = service
        .finish_opaque_password_update_after_passkey_verification(
            &user.id,
            &session_id,
            registration_upload.into(),
        )
        .await
        .checked("passkey-verified finish should accept pending passkey sessions");

    assert!(
        service
            .has_usable_password_authentication(&updated_user)
            .await
            .checked("password auth capability check should succeed"),
        "passkey-verified OPAQUE update must leave usable password authentication"
    );

    let old_password_login = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    assert!(
        old_password_login.is_err(),
        "old password must stop working after passkey-verified OPAQUE update"
    );

    let opaque_login_result = opaque_login_user(&service, username, "NewStrongPass1").await;
    assert!(
        opaque_login_result.is_ok(),
        "new OPAQUE credential must work after passkey-verified password update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_password_succeeds_even_when_family_revocation_store_fails() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FamilyRevocationFailingStore {
        inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
    });
    let service = create_user_service_with_blacklist(&pool, token_blacklist);

    let (user, username) = register_password_user_with_username(&service, "setpw_fail").await;

    let result = service.force_password_reset(&user.id).await;
    assert!(
        result.is_ok(),
        "Password reset should rely on version, not fail on best-effort family revocation persistence"
    );

    let login_old = opaque_login_user(&service, username.clone(), "StrongPass1").await;
    assert!(
        login_old.is_err(),
        "Old password must stop working after version is updated"
    );

    let login_new = opaque_login_user(&service, username.clone(), "AdminNewPass1").await;
    assert!(
        login_new.is_err(),
        "Forced reset should wait for the user-owned OPAQUE reset flow"
    );

    opaque_reset_password_after_external_verification(&service, &user.id, "AdminNewPass1")
        .await
        .checked("external verification should install replacement password");
    let opaque_login_new = opaque_login_user(&service, username, "AdminNewPass1").await;
    assert!(
        opaque_login_new.is_ok(),
        "New OPAQUE password must become active through reset flow"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_username_registration_and_login_are_case_insensitive() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let suffix = synctv_common::snanoid!(6);

    let (user, _, _) = opaque_register_user(
        &service,
        format!("CaseUser_{suffix}"),
        Some(format!("case_user_{suffix}@test.com")),
        "StrongPass1",
    )
    .await
    .checked("Registration should succeed");

    assert_eq!(
        user.username,
        format!("caseuser_{}", suffix.to_lowercase()),
        "Stored username should use the canonical lowercase form"
    );

    let duplicate = opaque_register_user(
        &service,
        format!("CASEUSER_{suffix}"),
        Some(format!("case_user_dup_{suffix}@test.com")),
        "StrongPass1",
    )
    .await;
    assert!(
        matches!(duplicate, Err(Error::AlreadyExists(_))),
        "Case variants of the same username must collide"
    );

    let (logged_in_user, _, _) = expect_complete_login(
        opaque_login_user(&service, format!("cAsEuSeR_{suffix}"), "StrongPass1")
            .await
            .checked("Login should accept case variants of the canonical username"),
    );
    assert_eq!(logged_in_user.id, user.id);

    let fetched = service
        .get_user_by_username(&format!("CASEUSER_{suffix}"))
        .await
        .checked("Case-insensitive username lookup should find the user");
    assert_eq!(fetched.id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_updates_username_only() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let new_username = format!("profile_atomic_new_{}", synctv_common::snanoid!(6));
    let (user, old_username) =
        register_password_user_with_username(&service, "profile_atomic").await;

    let before_version = load_password_credential_row(&pool, user.id).await.version;
    let updated_user = service
        .update_profile(&user.id, Some(new_username.to_uppercase()))
        .await
        .checked("Profile username update should succeed");
    let after_version = load_password_credential_row(&pool, user.id).await.version;

    assert_eq!(updated_user.username, new_username.to_lowercase());
    assert_eq!(
        after_version, before_version,
        "Username-only profile update must not increment version"
    );

    let login_old = opaque_login_user(&service, old_username, "StrongPass1").await;
    assert!(
        login_old.is_err(),
        "Old username must stop working after a successful profile update"
    );

    let login_new = opaque_login_user(&service, new_username.to_uppercase(), "StrongPass1").await;
    assert!(
        login_new.is_ok(),
        "Existing password must remain active with the new username after profile update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_rejects_empty_update() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);

    let (user, old_username) =
        register_password_user_with_username(&service, "profile_rollback").await;

    let result = service.update_profile(&user.id, None).await;

    assert!(
        matches!(result, Err(Error::InvalidInput(_))),
        "Empty profile update must be rejected"
    );

    let persisted = service
        .get_user(&user.id)
        .await
        .checked("User should still exist after rejected update");
    assert_eq!(
        persisted.username,
        old_username.to_lowercase(),
        "Username must not change when profile update is rejected"
    );

    let login_old = opaque_login_user(&service, old_username, "StrongPass1").await;
    assert!(
        login_old.is_ok(),
        "Original credentials must remain valid after a rejected combined profile update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_commits_when_family_revocation_store_fails() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FamilyRevocationFailingStore {
        inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
    });
    let service = create_user_service_with_blacklist(&pool, token_blacklist);

    let new_username = format!("profile_revoke_new_{}", synctv_common::snanoid!(6));
    let (user, old_username) =
        register_password_user_with_username(&service, "profile_revoke").await;

    let updated = service
        .update_profile(&user.id, Some(new_username.clone()))
        .await
        .checked("Profile update should commit");

    let persisted = service
        .get_user(&user.id)
        .await
        .checked("User should still exist after successful combined update");
    assert_eq!(
        persisted.username,
        new_username.to_lowercase(),
        "Username change must commit even when best-effort family revocation persistence fails"
    );
    assert_eq!(updated.username, persisted.username);

    let login_old = opaque_login_user(&service, old_username.clone(), "StrongPass1").await;
    assert!(
        login_old.is_err(),
        "Original credentials must stop working after a successful combined profile update"
    );

    let login_new = opaque_login_user(&service, new_username, "StrongPass1").await;
    assert!(
        login_new.is_ok(),
        "Existing password must remain active after a successful profile update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_register_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(&pool);

    let username = format!("cache_fail_register_{}", synctv_common::snanoid!(6));
    let (user, access_token, refresh_token) = opaque_register_user(
        &service,
        username.clone(),
        Some(format!(
            "cache_fail_register_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration must succeed even when username cache write fails");

    assert_eq!(user.username, username.to_lowercase());
    assert!(access_token.is_some());
    assert!(refresh_token.is_some());

    let persisted = service
        .get_user(&user.id)
        .await
        .checked("Registered user must be durable in the database");
    assert_eq!(persisted.username, user.username);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_finalize_registration_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(&pool);

    let user = create_user_with_password_fixture(
        &pool,
        format!("cache_fail_finalize_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "cache_fail_finalize_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("User creation should succeed");

    let (access_token, refresh_token) = service
        .finalize_registration(&user)
        .await
        .checked("Finalization must succeed even when username cache write fails");

    let jwt = create_jwt_service();
    assert!(jwt.verify_access_token(&access_token).is_ok());
    assert!(jwt.verify_refresh_token(&refresh_token).is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_with_role_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(&pool);

    let created = service
        .create_user_with_role(
            format!("cache_fail_admin_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "cache_fail_admin_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            Some(synctv_core::models::UserRole::Admin),
        )
        .await
        .checked("Admin user creation must succeed even when username cache write fails");

    let persisted = service
        .get_user(&created.id)
        .await
        .checked("Created admin user must be durable in the database");
    assert_eq!(persisted.id, created.id);
    assert_eq!(persisted.role, synctv_core::models::UserRole::Admin);
    assert_eq!(
        persisted.signup_method,
        synctv_core::models::SignupMethod::AdminCreated
    );

    let password_repository = UserPasswordRepository::new(pool.clone());
    let password_state = password_repository
        .get_state(&created.id)
        .await
        .checked("password state lookup should succeed");
    let has_opaque_credential = password_repository
        .has_opaque_credential(&created.id)
        .await
        .checked("password credential lookup should succeed");
    assert_eq!(password_state.version, 0);
    assert!(
        !has_opaque_credential,
        "admin-created users must wait for OPAQUE reset before credential material exists"
    );

    let password_login = opaque_login_user(&service, created.username.clone(), "StrongPass1").await;
    assert!(
        password_login.is_err(),
        "admin-created users should need OPAQUE reset before password login"
    );

    opaque_reset_password_after_external_verification(&service, &created.id, "StrongPass1")
        .await
        .checked("external verification should initialize admin-created password");
    let opaque_login_result =
        opaque_login_user(&service, created.username.clone(), "StrongPass1").await;
    assert!(
        opaque_login_result.is_ok(),
        "admin-created users must be able to use OPAQUE login"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_with_initial_banned_status_persists_ban_record() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let reviewer = service
        .create_user_with_role_and_status(
            format!("initial_banned_reviewer_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "initial_banned_reviewer_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            Some(synctv_core::models::UserRole::Admin),
            Some(synctv_core::models::UserStatus::Active),
            None,
        )
        .await
        .checked("reviewer should be created");

    let created = service
        .create_user_with_role_and_status(
            format!("initial_banned_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "initial_banned_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            Some(synctv_core::models::UserRole::User),
            Some(synctv_core::models::UserStatus::Banned),
            Some(&reviewer.id),
        )
        .await
        .checked("admin-created banned user should be created");

    assert_eq!(created.status, synctv_core::models::UserStatus::Banned);
    assert!(created.is_banned);

    let persisted = service
        .get_user(&created.id)
        .await
        .checked("created user should be durable");
    assert_eq!(persisted.status, synctv_core::models::UserStatus::Banned);
    assert_eq!(persisted.banned_by.as_ref(), Some(&reviewer.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_username_falls_back_to_database_when_cache_read_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(&pool);

    let (user, _, _) = opaque_register_user(
        &service,
        format!("cache_fail_lookup_{}", synctv_common::snanoid!(6)),
        Some(format!(
            "cache_fail_lookup_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        "StrongPass1",
    )
    .await
    .checked("Registration must succeed");

    let username = service
        .get_username(&user.id)
        .await
        .checked("Username lookup should fall back to database on cache read failure");

    assert_eq!(username.as_deref(), Some(user.username.as_str()));
}

// S13: create_or_load_by_oauth2

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_load_by_oauth2_normalizes_username_and_falls_back_to_provider_id() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let provider = OAuth2Provider::Google;

    let sanitized_user = service
        .create_or_load_by_oauth2(&provider, "provider_user_123", "user@special!chars.test")
        .await
        .checked("Should create user with sanitized username");

    assert!(
        sanitized_user
            .username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-'),
        "Username should be sanitized: {}",
        sanitized_user.username
    );
    assert!(
        !sanitized_user.username.contains('@'),
        "@ should be stripped from username"
    );
    assert!(
        !sanitized_user.username.contains('!'),
        "! should be stripped from username"
    );
    assert_eq!(
        sanitized_user.status,
        synctv_core::models::UserStatus::Active,
        "OAuth2-created users should start active so first login succeeds"
    );

    let fallback_user = service
        .create_or_load_by_oauth2(&provider, "fallback_provider_id", "@@@!!!")
        .await
        .checked("Should create user with fallback username");

    assert!(
        fallback_user.username.starts_with("user_"),
        "Empty sanitized username should fall back to 'user_<provider_id>': {}",
        fallback_user.username
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_load_by_oauth2_collision_retry() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let provider = OAuth2Provider::Google;

    let user1 = service
        .create_or_load_by_oauth2(&provider, "provider1", "oauth_user")
        .await
        .checked("First user creation should succeed");

    assert_eq!(user1.username, "oauth_user");

    let user2 = service
        .create_or_load_by_oauth2(&provider, "provider2", "oauth_user")
        .await
        .checked("Second user creation should succeed with suffixed username");

    assert_ne!(
        user2.username, "oauth_user",
        "Second user should have a different (suffixed) username"
    );
    assert!(
        user2.username.starts_with("oauth_user_"),
        "Suffixed username should start with 'oauth_user_': {}",
        user2.username
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_concurrent_requests_do_not_commit_orphan_oauth2_users() {
    let (_container, pool) = create_test_pool().await;
    let oauth_service = oauth2_service_with_google_signup(&pool).await;

    let provider = OAuth2Provider::Google;
    let user_info = synctv_core::service::OAuth2UserInfo {
        provider: provider.clone(),
        provider_instance_name: "google".to_string(),
        provider_issuer: Some("https://accounts.google.com".to_string()),
        provider_user_id: format!("oauth_concurrent_{}", synctv_common::snanoid!(8)),
        username: format!("oauth_concurrent_user_{}", synctv_common::snanoid!(6)),
        avatar: None,
    };

    let first = oauth_service.find_or_create_and_link("google", &user_info);
    let second = oauth_service.find_or_create_and_link("google", &user_info);
    let (first_result, second_result) = tokio::join!(first, second);

    let OAuth2LinkResult::Linked {
        user_id: first_user_id,
        ..
    } = first_result.checked("first concurrent login must succeed")
    else {
        std::panic::panic_any("first concurrent login should not require review");
    };
    let OAuth2LinkResult::Linked {
        user_id: second_user_id,
        ..
    } = second_result.checked("second concurrent login must succeed")
    else {
        std::panic::panic_any("second concurrent login should not require review");
    };
    assert_eq!(
        first_user_id, second_user_id,
        "Concurrent logins for the same provider identity must converge to one user"
    );

    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());
    let mapping = oauth_repo
        .find_by_provider_instance("google", &user_info.provider_user_id)
        .await
        .checked("mapping lookup must succeed")
        .checked("mapping must exist");
    assert_eq!(mapping.user_id, first_user_id);

    let user_repo = UserRepository::new(pool.clone());
    let oauth2_user_count: i64 = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM users u
        JOIN auth_oauth2_identities oc ON oc.user_id = u.id
        WHERE oc.provider_instance_name = $1
          AND oc.provider_user_id = $2
          AND u.deleted_at IS NULL
        "#,
        "google",
        &user_info.provider_user_id
    )
    .fetch_one(&pool)
    .await
    .checked("user count query must succeed");
    assert_eq!(
        oauth2_user_count, 1,
        "Concurrent OAuth2 signups must not commit an extra orphan user row"
    );

    let persisted_user = user_repo
        .get_by_id(&first_user_id)
        .await
        .checked("user lookup must succeed")
        .checked("winning user must exist");
    let persisted_email = synctv_core::repository::UserEmailRepository::new(pool.clone())
        .get_email(&first_user_id)
        .await
        .checked("email identity lookup must succeed");
    assert!(persisted_email.is_none());
    assert_eq!(
        persisted_user.status,
        synctv_core::models::UserStatus::Active,
        "OAuth2-created users must be active immediately"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_repeated_review_signup_returns_existing_pending_request() {
    let (_container, pool) = create_test_pool().await;
    let oauth_service = oauth2_service_with_github_review(&pool).await;

    let provider = OAuth2Provider::GitHub;
    let user_info = oauth2_user_info(
        provider.clone(),
        "github",
        format!("oauth_pending_{}", synctv_common::snanoid!(8)),
        format!("oauth_pending_user_{}", synctv_common::snanoid!(6)),
    );

    let first = oauth_service
        .find_or_create_and_link("github", &user_info)
        .await
        .checked("first OAuth2 review signup should create a pending request");
    let second = oauth_service
        .find_or_create_and_link("github", &user_info)
        .await
        .checked("repeated OAuth2 review signup should return existing pending request");

    let OAuth2LinkResult::PendingReview(first_pending) = first else {
        std::panic::panic_any("first OAuth2 review signup should require review");
    };
    let OAuth2LinkResult::PendingReview(second_pending) = second else {
        std::panic::panic_any("repeated OAuth2 review signup should require review");
    };

    assert_eq!(
        first_pending.request_id, second_pending.request_id,
        "repeated pending OAuth2 signup should converge to the original review request"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_review_signup_skips_existing_usernames() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);
    let oauth_service = oauth2_service_with_github_review(&pool).await;

    opaque_register_user(
        &user_service,
        "oauth_review_collision_user",
        Some("oauth_review_collision_local@test.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("seed local user should be created");

    let provider = OAuth2Provider::GitHub;
    let user_info = oauth2_user_info(
        provider.clone(),
        "github",
        format!("oauth_review_collision_{}", synctv_common::snanoid!(8)),
        "oauth_review_collision_user",
    );

    let OAuth2LinkResult::PendingReview(pending) = oauth_service
        .find_or_create_and_link("github", &user_info)
        .await
        .checked("OAuth2 review signup should create a pending request with a suffixed username")
    else {
        std::panic::panic_any("OAuth2 signup should require review in this test");
    };

    let pending_username: String = sqlx::query_scalar!(
        r#"
        SELECT username AS "username!"
        FROM user_registration_requests
        WHERE id = $1
        "#,
        pending.request_id.as_i64()
    )
    .fetch_one(&pool)
    .await
    .checked("pending registration request should exist");

    assert_ne!(pending_username, "oauth_review_collision_user");
    assert!(
        pending_username.starts_with("oauth_review_collision_user_"),
        "expected suffixed pending username, got {pending_username}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_retries_with_suffixed_username_on_collision() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(&pool);
    let oauth_service = oauth2_service_with_google_signup(&pool).await;

    opaque_register_user(
        &user_service,
        "oauth_collision_user",
        Some("local_collision@test.com".to_string()),
        "StrongPass1",
    )
    .await
    .checked("seed local user should be created");

    let provider = OAuth2Provider::Google;
    let user_info = oauth2_user_info(
        provider.clone(),
        "google",
        format!("oauth_collision_{}", synctv_common::snanoid!(8)),
        "oauth_collision_user",
    );

    let OAuth2LinkResult::Linked {
        user_id: created_user_id,
        is_new,
    } = oauth_service
        .find_or_create_and_link("google", &user_info)
        .await
        .checked("OAuth2 signup should succeed by choosing a suffixed username")
    else {
        std::panic::panic_any("OAuth2 signup should not require review in this test");
    };

    assert!(is_new, "first OAuth2 login should create a new user");

    let user_repo = UserRepository::new(pool.clone());
    let created_user = user_repo
        .get_by_id(&created_user_id)
        .await
        .checked("user lookup should succeed")
        .checked("created OAuth2 user should exist");

    assert_ne!(created_user.username, "oauth_collision_user");
    assert!(
        created_user.username.starts_with("oauth_collision_user_"),
        "expected suffixed username, got {}",
        created_user.username
    );
    assert_eq!(
        created_user.signup_method,
        synctv_core::models::SignupMethod::OAuth2
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rate_limiting_per_user() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(&pool);
    let refresh_token = register_password_user_refresh_token(&service, "rate_limit_refresh").await;

    let mut success_count = 0;
    let mut rate_limited = false;
    let mut current_token = refresh_token;

    for _ in 0..20 {
        match service.refresh_token(current_token.clone()).await {
            Ok((_new_access, new_refresh)) => {
                success_count += 1;
                current_token = new_refresh;
            }
            Err(Error::RateLimited(_)) => {
                rate_limited = true;
                break;
            }
            Err(e) => {
                std::panic::panic_any(format!("unexpected error during refresh: {e:?}"));
            }
        }
    }

    assert!(
        rate_limited,
        "Refresh token endpoint should be rate limited after {success_count} requests"
    );

    assert!(
        success_count > 0,
        "Should allow at least some refresh requests before rate limiting"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_concurrent_refresh_race_condition() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_in_memory_blacklist(&pool);
    let refresh_token = register_password_user_refresh_token(&service, "concurrent_race").await;

    let successes = run_concurrent_refresh_attempts(service.clone(), refresh_token, 10).await;
    let failures = 10 - successes;

    assert_eq!(
        successes, 1,
        "Exactly ONE concurrent refresh should succeed, got {successes} successes and {failures} failures. \
         Multiple successful refreshes would mean refresh-token replay detection failed."
    );
    assert_eq!(
        failures, 9,
        "Nine requests should fail due to JTI already blacklisted"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_concurrent_refresh_family_revocation() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_in_memory_blacklist(&pool);
    let refresh_token = register_password_user_refresh_token(&service, "family_rev_race").await;

    let (_access1, refresh_token1) = service
        .refresh_token(refresh_token.clone())
        .await
        .checked("First refresh should succeed");

    let _successes = run_concurrent_refresh_attempts(service.clone(), refresh_token, 5).await;

    let result = service.refresh_token(refresh_token1).await;
    assert!(
        result.is_err(),
        "New token should be blocked after family revocation from concurrent replay detection"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rate_limit_recovers() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let mut runtime = default_test_user_runtime_options();
    runtime.refresh_rate_limiter = Arc::new(RateLimiter::local_only(
        "test-refresh-recover-short-window:".to_string(),
    ));
    runtime.refresh_rate_limit_config = synctv_core::service::RefreshRateLimitConfig {
        requests: 1,
        window_secs: 1,
    };
    let service =
        create_user_service_with_components(&pool, username_cache, token_blacklist, runtime);
    let refresh_token = register_password_user_refresh_token(&service, "rate_limit_recover").await;

    // Using 1 request / 1 second preserves the recovery semantics while
    // avoiding a real 7 second sleep in the test.
    let mut current_token = refresh_token;
    for _ in 0..2 {
        match service.refresh_token(current_token.clone()).await {
            Ok((_access, new_refresh)) => {
                current_token = new_refresh;
            }
            Err(_) => break,
        }
    }

    let result = service.refresh_token(current_token.clone()).await;
    assert!(
        result.is_err(),
        "Refresh should be rate limited before the short window resets"
    );

    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let result = service.refresh_token(current_token).await;
    assert!(
        result.is_ok(),
        "Should be able to refresh again after rate limit window resets: {:?}",
        result.err()
    );
}

// S2.6: Password timing attack prevention tests
