//! Authentication integration tests
//!
//! Tests the complete authentication flow: register -> login -> operations -> logout
//! with all security checks enforced by `SecurityPipeline`.
//!
//! Run with: cargo test --test `auth_integration_tests`
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

use sqlx::PgPool;
use synctv_core::{
    cache,
    config::PasswordComplexityConfig,
    models::UserStatus,
    repository::UserRepository,
    service::{
        auth::{jwt::JwtService, SecurityPipeline, TestPasswordHasher},
        AuthenticatedLogin, BruteForceProtection, InMemoryTokenBlacklistStore, TokenBlacklistStore,
        UserService,
    },
    Error, KeyBuilder,
};
use synctv_core_testing::create_test_pool;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc.enable_password_registration_for_tests();
    svc
}

fn expect_complete_login(login: AuthenticatedLogin) -> (synctv_core::models::User, String, String) {
    match login {
        AuthenticatedLogin::Complete {
            user,
            access_token,
            refresh_token,
        } => (user, access_token, refresh_token),
        AuthenticatedLogin::MfaRequired { .. } => {
            panic!("expected complete login, got MFA challenge")
        }
    }
}

// Test 1: Password Change Invalidates Old Tokens

async fn scenario_password_change_invalidates_old_tokens() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(pool));
    let jwt_service = create_jwt_service();

    let username = format!("user_{}", synctv_common::snanoid!(8));
    let email = format!("{}@test.com", synctv_common::snanoid!(8));
    let original_password = "OriginalPassword123!".to_string();

    let (user, _, _) = user_service
        .register(
            username.clone(),
            Some(email),
            original_password.clone(),
            None,
        )
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = expect_complete_login(
        user_service
            .login(username.clone(), original_password.clone(), None)
            .await
            .expect("Failed to login"),
    );

    let old_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify old token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

    let auth_result = pipeline.check(&old_claims).await;
    assert!(
        auth_result.is_ok(),
        "Old token should work before password change"
    );

    let new_password = "NewPassword456!";
    user_service
        .change_password(&user.id, &original_password, new_password)
        .await
        .expect("Failed to change password");

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

    let (_user, new_access_token, _new_refresh_token) = expect_complete_login(
        user_service
            .login(username, new_password.to_string(), None)
            .await
            .expect("Failed to login with new password"),
    );

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify new token");

    let auth_result = pipeline.check(&new_claims).await;
    assert!(
        auth_result.is_ok(),
        "New token should work after password change"
    );
}

// Test 2: User Ban Invalidates Tokens

async fn scenario_ban_user_invalidates_tokens() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    let username = format!("banned_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = expect_complete_login(
        user_service
            .login(username.clone(), password, None)
            .await
            .expect("Failed to login"),
    );

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

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
    let user_service = Arc::new(create_user_service(pool));
    let jwt_service = create_jwt_service();

    let username = format!("blacklist_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = expect_complete_login(
        user_service
            .login(username, password, None)
            .await
            .expect("Failed to login"),
    );

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist.clone(), key_builder.clone());

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
    let user_service = Arc::new(create_user_service(pool));

    // Register and login user
    let username = format!("refresh_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, _access_token, refresh_token) = expect_complete_login(
        user_service
            .login(username, password, None)
            .await
            .expect("Failed to login"),
    );

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
    let user_service = Arc::new(create_user_service(pool));
    let jwt_service = create_jwt_service();

    let username = format!("e2e_user_{}", synctv_common::snanoid!(8));
    let email = format!("{}@test.com", synctv_common::snanoid!(8));
    let password = "SecurePassword123!".to_string();

    let (user, _, _) = user_service
        .register(username.clone(), Some(email), password.clone(), None)
        .await
        .expect("Failed to register user");

    assert_eq!(user.username, username);
    assert_eq!(user.status, UserStatus::Active);

    let (_user, access_token, refresh_token) = expect_complete_login(
        user_service
            .login(username.clone(), password.clone(), None)
            .await
            .expect("Failed to login"),
    );

    assert!(!access_token.is_empty());
    assert!(!refresh_token.is_empty());

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");

    assert_eq!(claims.user_id().unwrap(), user.id);

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

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
    let user_service = Arc::new(create_user_service(pool));

    // Register user
    let username = format!("wrong_pwd_user_{}", synctv_common::snanoid!(8));
    let password = "CorrectPassword123!".to_string();

    user_service
        .register(username.clone(), None, password, None)
        .await
        .expect("Failed to register user");

    // Try to login with wrong password
    let login_result = user_service
        .login(username, "WrongPassword456!".to_string(), None)
        .await;

    assert!(
        login_result.is_err(),
        "Login should fail with wrong password"
    );
}

async fn scenario_deleted_user_cannot_authenticate() {
    let (_postgres, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // Register and login user
    let username = format!("deleted_user_{}", synctv_common::snanoid!(8));
    let password = "Password123!".to_string();

    let (user, _, _) = user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = expect_complete_login(
        user_service
            .login(username, password, None)
            .await
            .expect("Failed to login"),
    );

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
    let pipeline =
        SecurityPipeline::new(user_service).with_token_blacklist(token_blacklist, key_builder);

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
