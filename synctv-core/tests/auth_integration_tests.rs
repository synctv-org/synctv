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
    cache::{self, NoopCacheL2},
    config::PasswordComplexityConfig,
    models::UserStatus,
    repository::UserRepository,
    service::{
        auth::{jwt::JwtService, SecurityPipeline},
        BruteForceProtection, InMemoryTokenBlacklistStore, TokenBlacklistStore, UserService,
    },
    Error, KeyBuilder,
};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;

const POSTGRES_VERSION: &str = "16-alpine";

// ============================================================================
// Test Infrastructure
// ============================================================================

async fn create_test_infra() -> (
    ContainerAsync<Postgres>,
    ContainerAsync<Redis>,
    PgPool,
    String,
) {
    use testcontainers::core::ImageExt;

    // Start PostgreSQL
    let postgres = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Postgres::default()
            .with_db_name("synctv_test")
            .with_user("synctv")
            .with_password("synctv_test")
            .with_tag(POSTGRES_VERSION)
            .start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Postgres container");

    let pg_host = postgres.get_host().await.expect("Failed to get host");
    let pg_port = postgres
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");

    let database_url = format!("postgres://synctv:synctv_test@{pg_host}:{pg_port}/synctv_test");

    // Retry connection until PG is fully ready
    let pool = {
        let mut retries = 0u32;
        loop {
            match sqlx::postgres::PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(2))
                .connect(&database_url)
                .await
            {
                Ok(p) => break p,
                Err(_) if retries < 60 => {
                    retries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(e) => panic!("PostgreSQL not ready after {retries} retries: {e}"),
            }
        }
    };

    // Run migrations
    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    // Start Redis
    let redis = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Redis::default().start(),
    )
    .await
    .expect("Docker container startup timed out (is Docker running?)")
    .expect("Failed to start Redis container");

    let redis_port = redis
        .get_host_port_ipv4(6379)
        .await
        .expect("Failed to get port");
    let redis_url = format!("redis://127.0.0.1:{redis_port}");

    (postgres, redis, pool, redis_url)
}

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache =
        cache::UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 1000, 0);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        key_builder,
        brute_force,
    )
}

// ============================================================================
// Test 1: Password Change Invalidates Old Tokens
// ============================================================================

/// Test that changing password invalidates old tokens.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_change_invalidates_old_tokens() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // 1. Register user
    let username = format!("user_{}", nanoid::nanoid!(8));
    let email = format!("{}@test.com", nanoid::nanoid!(8));
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

    // 2. Login and get access token
    let (_user, access_token, _refresh_token) = user_service
        .login(username.clone(), original_password.clone(), None)
        .await
        .expect("Failed to login");

    let old_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify old token");

    // 3. Create security pipeline and verify old token works
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

    let auth_result = pipeline.check(&old_claims).await;
    assert!(
        auth_result.is_ok(),
        "Old token should work before password change"
    );

    // 4. Change password
    let new_password = "NewPassword456!";
    user_service
        .change_password(&user.id, &original_password, new_password)
        .await
        .expect("Failed to change password");

    // 5. Verify old token is rejected (password version mismatch)
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

    // 6. Login again with new password
    let (_user, new_access_token, _new_refresh_token) = user_service
        .login(username, new_password.to_string(), None)
        .await
        .expect("Failed to login with new password");

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify new token");

    // 7. Verify new token works
    let auth_result = pipeline.check(&new_claims).await;
    assert!(
        auth_result.is_ok(),
        "New token should work after password change"
    );
}

// ============================================================================
// Test 2: User Ban Invalidates Tokens
// ============================================================================

/// Test that banning a user invalidates their tokens.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_ban_user_invalidates_tokens() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // 1. Register and login user
    let username = format!("banned_user_{}", nanoid::nanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = user_service
        .login(username.clone(), password, None)
        .await
        .expect("Failed to login");

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    // 2. Create security pipeline and verify token works
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_ok(), "Token should work before ban");

    // 3. Admin bans user (simulate via DB update)
    let user_repo = UserRepository::new(pool);
    let user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to get user")
        .expect("User should exist");
    user_repo
        .update_status(&user.id, UserStatus::Banned)
        .await
        .expect("Failed to ban user");

    // 4. Verify token is rejected
    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_err(), "Token should be rejected after ban");

    let err = auth_result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(_)),
        "Should be an Authentication error, got: {err}"
    );
}

// ============================================================================
// Test 3: Access Token Blacklist
// ============================================================================

/// Test that blacklisted access tokens are rejected.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blacklisted_access_token_rejected() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // 1. Register and login user
    let username = format!("blacklist_user_{}", nanoid::nanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = user_service
        .login(username, password, None)
        .await
        .expect("Failed to login");

    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify token");

    // 2. Create security pipeline and verify token works
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist.clone(), key_builder.clone());

    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_ok(), "Token should work before blacklisting");

    // 3. Blacklist the access token (simulating logout)
    let blacklist_key = key_builder.access_token_blacklist(&claims.jti);
    token_blacklist
        .blacklist(&blacklist_key, 3600)
        .await
        .expect("Failed to blacklist token");

    // 4. Verify access token is rejected
    let auth_result = pipeline.check(&claims).await;
    assert!(auth_result.is_err(), "Blacklisted token should be rejected");

    let err = auth_result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(_)),
        "Should be an Authentication error, got: {err}"
    );
}

/// Test that refresh token validation works.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_validation() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool));

    // Register and login user
    let username = format!("refresh_user_{}", nanoid::nanoid!(8));
    let password = "Password123!".to_string();

    user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, _access_token, refresh_token) = user_service
        .login(username, password, None)
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

// ============================================================================
// Test 4: Complete Authentication Flow
// ============================================================================

/// Test the complete authentication flow from registration to token refresh.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_complete_authentication_flow() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // Step 1: Register new user
    let username = format!("e2e_user_{}", nanoid::nanoid!(8));
    let email = format!("{}@test.com", nanoid::nanoid!(8));
    let password = "SecurePassword123!".to_string();

    let (user, _, _) = user_service
        .register(username.clone(), Some(email), password.clone(), None)
        .await
        .expect("Failed to register user");

    assert_eq!(user.username, username);
    assert_eq!(user.status, UserStatus::Active);

    // Step 2: Login with credentials
    let (_user, access_token, refresh_token) = user_service
        .login(username.clone(), password.clone(), None)
        .await
        .expect("Failed to login");

    assert!(!access_token.is_empty());
    assert!(!refresh_token.is_empty());

    // Step 3: Verify access token via SecurityPipeline
    let claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");

    assert_eq!(claims.user_id(), user.id);

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let pipeline = SecurityPipeline::new(user_service.clone())
        .with_token_blacklist(token_blacklist, key_builder);

    let auth_result = pipeline
        .check(&claims)
        .await
        .expect("Security check failed");
    assert_eq!(auth_result.user_id, user.id);

    // Step 4: Refresh access token
    let (new_access_token, _new_refresh_token) = user_service
        .refresh_token(refresh_token.clone())
        .await
        .expect("Failed to refresh token");

    assert!(!new_access_token.is_empty());

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify refreshed token");

    // Step 5: Verify new token works
    let auth_result = pipeline
        .check(&new_claims)
        .await
        .expect("New token should work");
    assert_eq!(auth_result.user_id, user.id);
}

/// Test login with wrong password fails.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_wrong_password_fails() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool));

    // Register user
    let username = format!("wrong_pwd_user_{}", nanoid::nanoid!(8));
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

/// Test that deleted user cannot authenticate.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_user_cannot_authenticate() {
    let (_postgres, _redis, pool, _redis_url) = create_test_infra().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let jwt_service = create_jwt_service();

    // Register and login user
    let username = format!("deleted_user_{}", nanoid::nanoid!(8));
    let password = "Password123!".to_string();

    let (user, _, _) = user_service
        .register(username.clone(), None, password.clone(), None)
        .await
        .expect("Failed to register user");

    let (_user, access_token, _refresh_token) = user_service
        .login(username, password, None)
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

    // Create security pipeline
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
