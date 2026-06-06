//! Authentication integration tests
//!
//! Tests the complete authentication flow: register -> login -> operations -> logout
//! with all security checks enforced by `SecurityPipeline`.
//!
//!
//! # Test Coverage
//!
//! - Password change invalidates old tokens
//! - User ban invalidates tokens
//! - Logout blacklists access tokens
//! - Complete login/logout flow
//!
//! # Requirements
//!
//! - Docker for testcontainers (`PostgreSQL` + Redis)
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use opaque_ke::argon2::Argon2 as OpaqueArgon2Ksf;
use opaque_ke::ciphersuite::CipherSuite;
use opaque_ke::rand::rngs::OsRng;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse,
};
use sqlx::PgPool;
use synctv_core::{
    cache,
    models::UserStatus,
    repository::UserRepository,
    service::{
        auth::{jwt::JwtService, SecurityPipeline, SecurityPipelineRuntime},
        AccountRegistrationOutcome, AuthenticatedLogin, BruteForceProtection,
        InMemoryTokenBlacklistStore, TokenBlacklistStore, UserService,
    },
    Error, KeyBuilder,
};
use synctv_core_testing::create_test_pool;

struct TestOpaqueCipherSuite;

impl CipherSuite for TestOpaqueCipherSuite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2_010::Sha512>;
    type Ksf = OpaqueArgon2Ksf<'static>;
}

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

fn create_user_service(pool: &PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_with_runtime(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
        synctv_core::service::user::UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..synctv_core::service::user::UserServiceRuntimeOptions::test_defaults()
        },
    )
}

fn security_pipeline_with_blacklist(
    user_service: Arc<UserService>,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    key_builder: KeyBuilder,
) -> SecurityPipeline {
    SecurityPipeline::new_with_runtime(
        user_service,
        SecurityPipelineRuntime {
            user_cache: None,
            token_blacklist: Some(token_blacklist),
            key_builder: Some(key_builder),
        },
    )
}

async fn opaque_login(
    service: &UserService,
    identifier: String,
    password: &str,
) -> synctv_core::Result<(synctv_core::models::User, String, String)> {
    let mut rng = OsRng;
    let client_start = ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
        .expect("client OPAQUE login start should succeed");
    let challenge = service
        .start_opaque_login_with_control(
            identifier,
            client_start.message.serialize().to_vec(),
            None,
            None,
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
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
            client_finish.message.serialize().to_vec(),
            None,
            None,
        )
        .await?;
    match login {
        AuthenticatedLogin::Complete {
            user,
            access_token,
            refresh_token,
            ..
        } => Ok((user, access_token, refresh_token)),
        AuthenticatedLogin::MfaRequired { .. } => Err(Error::Authentication(
            "Unexpected MFA challenge in opaque_login test helper".to_string(),
        )),
    }
}

async fn opaque_register(
    service: &UserService,
    username: String,
    email: Option<String>,
    password: &str,
) -> synctv_core::Result<(synctv_core::models::User, Option<String>, Option<String>)> {
    let mut rng = OsRng;
    let client_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let challenge = service
        .start_opaque_registration_with_control(
            username,
            email,
            client_start.message.serialize().to_vec(),
            None,
            None,
        )
        .await?;
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .expect("server registration response should deserialize");
    let client_finish = client_start
        .state
        .finish(
            &mut rng,
            password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

    match service
        .finish_opaque_registration_with_control(
            &challenge.session_id,
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
        Ok(AccountRegistrationOutcome::PendingReview(_)) => Err(Error::Internal(
            "opaque_register helper received pending review outcome".to_string(),
        )),
        Err(error) => Err(error),
    }
}

async fn opaque_update_password(
    service: &UserService,
    user_id: &synctv_core::models::UserId,
    old_password: &str,
    new_password: &str,
) -> synctv_core::Result<synctv_core::models::User> {
    let mut rng = OsRng;
    let login_start =
        ClientLogin::<TestOpaqueCipherSuite>::start(&mut rng, old_password.as_bytes())
            .expect("client OPAQUE login start should succeed");
    let registration_start =
        ClientRegistration::<TestOpaqueCipherSuite>::start(&mut rng, new_password.as_bytes())
            .expect("client OPAQUE registration start should succeed");
    let challenge = service
        .start_opaque_password_update(
            user_id,
            login_start.message.serialize().to_vec(),
            registration_start.message.serialize().to_vec(),
        )
        .await?;
    let credential_response =
        CredentialResponse::<TestOpaqueCipherSuite>::deserialize(&challenge.credential_response)
            .expect("server credential response should deserialize");
    let login_finish = login_start
        .state
        .finish(
            &mut rng,
            old_password.as_bytes(),
            credential_response,
            ClientLoginFinishParameters::default(),
        )
        .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
    let registration_response = RegistrationResponse::<TestOpaqueCipherSuite>::deserialize(
        &challenge.registration_response,
    )
    .expect("server registration response should deserialize");
    let registration_finish = registration_start
        .state
        .finish(
            &mut rng,
            new_password.as_bytes(),
            registration_response,
            ClientRegistrationFinishParameters::default(),
        )
        .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;

    service
        .finish_opaque_password_update(
            user_id,
            &challenge.session_id,
            login_finish.message.serialize().to_vec(),
            registration_finish.message.serialize().to_vec(),
        )
        .await
}

// Test 1: Password Change Invalidates Old Tokens

async fn scenario_password_change_invalidates_old_tokens() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let jwt_service = create_jwt_service();

    let username = format!("user_{}", synctv_common::snanoid!(8));
    let email = format!("{}@test.com", synctv_common::snanoid!(8));
    let original_password = "OriginalPassword123!".to_string();

    let (user, _, _) = opaque_register(
        &user_service,
        username.clone(),
        Some(email),
        &original_password,
    )
    .await
    .expect("Failed to register user");

    let (_user, access_token, _refresh_token) =
        opaque_login(&user_service, username.clone(), &original_password)
            .await
            .expect("Failed to login");

    let old_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify old token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline =
        security_pipeline_with_blacklist(user_service.clone(), token_blacklist, key_builder);

    let auth_result = pipeline.check(&old_claims).await;
    assert!(
        auth_result.is_ok(),
        "Old token should work before password change"
    );

    let new_password = "NewPassword456!";
    opaque_update_password(&user_service, &user.id, &original_password, new_password)
        .await
        .expect("Failed to change password through OPAQUE update");

    let auth_result = pipeline.check(&old_claims).await;
    assert!(
        auth_result.is_err(),
        "Old token should be rejected after password change"
    );

    let err = auth_result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("password change")),
        "Error should mention password change, got: {err}"
    );

    let (_user, new_access_token, _new_refresh_token) =
        opaque_login(&user_service, username, new_password)
            .await
            .expect("Failed to login with new OPAQUE password");

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify new token");

    let auth_result = pipeline.check(&new_claims).await;
    assert!(
        auth_result.is_ok(),
        "New token should work after password change"
    );
}

async fn scenario_opaque_registration_allows_opaque_login() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));

    let username = format!("opaque_user_{}", synctv_common::snanoid!(8));
    let password = "OpaquePassword123!";

    opaque_register(&user_service, username.clone(), None, password)
        .await
        .expect("Failed to register user with OPAQUE");

    opaque_login(&user_service, username, password)
        .await
        .expect("Failed to login with OPAQUE password after OPAQUE registration");
}

async fn scenario_opaque_password_update_allows_opaque_login() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));

    let username = format!("opaque_update_user_{}", synctv_common::snanoid!(8));
    let old_password = "OldOpaquePassword123!";
    let new_password = "NewOpaquePassword456!";
    let (user, _, _) = opaque_register(&user_service, username.clone(), None, old_password)
        .await
        .expect("Failed to register user with OPAQUE");

    opaque_update_password(&user_service, &user.id, old_password, new_password)
        .await
        .expect("Failed to update password with OPAQUE");

    opaque_login(&user_service, username, new_password)
        .await
        .expect("Failed to login with OPAQUE password after OPAQUE update");
}

// Test 2: User Ban Invalidates Tokens

async fn scenario_ban_user_invalidates_tokens() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let jwt_service = create_jwt_service();

    let username = format!("banned_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    opaque_register(&user_service, username.clone(), None, &password)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) =
        opaque_login(&user_service, username.clone(), &password)
            .await
            .expect("Failed to login");

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline =
        security_pipeline_with_blacklist(user_service.clone(), token_blacklist, key_builder);

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_ok(), "Token should work before ban");

    let user_repo = UserRepository::new(pool);
    let user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to get user")
        .expect("User should exist");
    user_repo
        .ban(&user.id, None, Some("auth integration test".to_string()))
        .await
        .expect("Failed to ban user");

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_err(), "Token should be rejected after ban");

    let err = auth_result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(_)),
        "Should be an Authentication error, got: {err}"
    );
}

// Test 3: Access Token Blacklist

async fn scenario_blacklisted_access_token_rejected() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let jwt_service = create_jwt_service();

    let username = format!("blacklist_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    opaque_register(&user_service, username.clone(), None, &password)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = opaque_login(&user_service, username, &password)
        .await
        .expect("Failed to login");

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = security_pipeline_with_blacklist(
        user_service.clone(),
        token_blacklist.clone(),
        key_builder.clone(),
    );

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_ok(), "Token should work before blacklisting");

    let blacklist_key = key_builder.access_token_blacklist(&claims.jti);
    token_blacklist
        .blacklist(&blacklist_key, 3600)
        .await
        .expect("Failed to blacklist token");

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_err(), "Blacklisted token should be rejected");

    let err = auth_result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(_)),
        "Should be an Authentication error, got: {err}"
    );
}

async fn scenario_refresh_token_validation() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));

    // Register and login user
    let username = format!("refresh_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    opaque_register(&user_service, username.clone(), None, &password)
        .await
        .expect("Failed to register user");

    let (_user, _access_token, refresh_token) = opaque_login(&user_service, username, &password)
        .await
        .expect("Failed to login");

    // Refresh token should work
    let refresh_result = user_service.refresh_token(refresh_token).await;
    assert!(refresh_result.is_ok(), "Refresh token should be valid");

    // Try with invalid refresh token
    let invalid_refresh_result = user_service
        .refresh_token("invalid_token".to_string())
        .await;
    assert!(
        invalid_refresh_result.is_err(),
        "Invalid refresh token should fail"
    );
}

// Test 4: Complete Authentication Flow

async fn scenario_complete_authentication_flow() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let jwt_service = create_jwt_service();

    let username = format!("e2e_user_{}", synctv_common::snanoid!(8));
    let email = format!("{}@test.com", synctv_common::snanoid!(8));
    let password = "SecurePassword123!".to_string();

    let (user, _, _) = opaque_register(&user_service, username.clone(), Some(email), &password)
        .await
        .expect("Failed to register user");

    assert_eq!(user.username, username.to_lowercase());
    assert_eq!(user.status, UserStatus::Active);

    let (_user, access_token, refresh_token) =
        opaque_login(&user_service, username.clone(), &password)
            .await
            .expect("Failed to login");

    assert!(!access_token.is_empty());
    assert!(!refresh_token.is_empty());

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");

    assert_eq!(claims.user_id().unwrap(), user.id);

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline =
        security_pipeline_with_blacklist(user_service.clone(), token_blacklist, key_builder);

    let auth_result = pipeline
        .check(&claims)
        .await
        .expect("Security check failed");
    assert_eq!(auth_result.user_id, user.id);

    let (new_access_token, _new_refresh_token) = user_service
        .refresh_token(refresh_token.clone())
        .await
        .expect("Failed to refresh token");

    assert!(!new_access_token.is_empty());

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify refreshed token");

    let auth_result = pipeline
        .check(&new_claims)
        .await
        .expect("New token should work");
    assert_eq!(auth_result.user_id, user.id);
}

async fn scenario_login_wrong_password_fails() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));

    // Register user
    let username = format!("wrong_pwd_user_{}", synctv_common::snanoid!(8));
    let password = "CorrectPassword123!".to_string();

    opaque_register(&user_service, username.clone(), None, &password)
        .await
        .expect("Failed to register user");

    let login_result = opaque_login(&user_service, username, "WrongPassword456!").await;

    assert!(
        login_result.is_err(),
        "Login should fail with wrong password"
    );
}

async fn scenario_deleted_user_cannot_authenticate() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let jwt_service = create_jwt_service();

    // Register and login user
    let username = format!("deleted_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    let (user, _, _) = opaque_register(&user_service, username.clone(), None, &password)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = opaque_login(&user_service, username, &password)
        .await
        .expect("Failed to login");

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    // Delete user
    let user_repo = UserRepository::new(pool);
    user_repo
        .delete(&user.id)
        .await
        .expect("Failed to delete user");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = security_pipeline_with_blacklist(user_service, token_blacklist, key_builder);

    // Token should be rejected because user is deleted
    let auth_result = pipeline.check(&claims).await;
    assert!(
        auth_result.is_err(),
        "Deleted user token should be rejected"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_change_invalidates_old_tokens() {
    scenario_password_change_invalidates_old_tokens().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_invalidates_tokens() {
    scenario_ban_user_invalidates_tokens().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blacklisted_access_token_rejected() {
    scenario_blacklisted_access_token_rejected().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_validation() {
    scenario_refresh_token_validation().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_complete_authentication_flow() {
    scenario_complete_authentication_flow().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_wrong_password_fails() {
    scenario_login_wrong_password_fails().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_user_cannot_authenticate() {
    scenario_deleted_user_cannot_authenticate().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_registration_allows_opaque_login() {
    scenario_opaque_registration_allows_opaque_login().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_opaque_password_update_allows_opaque_login() {
    scenario_opaque_password_update_allows_opaque_login().await;
}
