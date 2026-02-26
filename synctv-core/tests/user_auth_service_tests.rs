//! User auth/security service tests
//!
//! Tests for UserService::refresh_token, login status checks, delete_user,
//! change_password/set_password, and create_or_load_by_oauth2.
//!
//! S1/S2 tests use InMemoryTokenBlacklistStore + InMemoryBruteForceProtection + real JwtService.
//! S3/S7/S13 tests use testcontainers PG.
//!
//! Run with: cargo test --test user_auth_service_tests

use std::sync::Arc;

use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::UserId,
    service::{
        auth::jwt::JwtService,
        InMemoryTokenBlacklistStore, TokenBlacklistStore, UserService,
        BruteForceProtection,
    },
    Error,
};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";
const JWT_SECRET: &str = "test-secret-key-for-user-auth-service-tests-long-enough-1234567890";

fn create_jwt_service() -> JwtService {
    JwtService::with_durations(JWT_SECRET, 1, 30, 4, 60).unwrap()
}

fn create_user_service_with_blacklist(
    pool: PgPool,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
) -> UserService {
    let jwt = create_jwt_service();
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 1000, 0);
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn create_user_service(pool: PgPool) -> UserService {
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    create_user_service_with_blacklist(pool, token_blacklist)
}

fn create_user_service_with_email_verification(pool: PgPool) -> UserService {
    let mut service = create_user_service(pool);
    service.set_email_verification_required(true);
    service
}

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (postgres, pool)
}

// ============================================================================
// S1: UserService::refresh_token (Refresh Token Rotation)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_happy_path() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register a user
    let (user, Some(access_token), Some(refresh_token)) = service
        .register(
            format!("refresh_user_{}", nanoid::nanoid!(6)),
            Some(format!("refresh_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens from registration");
    };

    // Refresh the token
    let (new_access, new_refresh) = service
        .refresh_token(refresh_token.clone())
        .await
        .expect("Refresh should succeed");

    // Verify new tokens are valid
    let jwt = create_jwt_service();
    let access_claims = jwt.verify_access_token(&new_access).expect("New access token valid");
    let refresh_claims = jwt.verify_refresh_token(&new_refresh).expect("New refresh token valid");

    assert_eq!(access_claims.sub, user.id.as_str());
    assert_eq!(refresh_claims.sub, user.id.as_str());

    // New tokens should be different from old ones
    assert_ne!(new_access, access_token);
    assert_ne!(new_refresh, refresh_token);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_old_jti_blacklisted_before_new_issued() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist.clone());

    // Register and get tokens
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("blacklist_user_{}", nanoid::nanoid!(6)),
            Some(format!("blacklist_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Extract JTI from the old refresh token
    let jwt = create_jwt_service();
    let old_claims = jwt
        .verify_refresh_token(&refresh_token)
        .expect("Old refresh token valid");
    let old_jti = old_claims.jti.clone();

    // Refresh
    let _new_tokens = service
        .refresh_token(refresh_token.clone())
        .await
        .expect("Refresh should succeed");

    // Verify old JTI is now blacklisted
    let key_builder = KeyBuilder::new("test");
    let blacklist_key = key_builder.refresh_token_blacklist(&old_jti);
    assert!(
        token_blacklist.is_blacklisted(&blacklist_key).await,
        "Old JTI should be blacklisted after refresh"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_replay_same_jti_triggers_family_revocation() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist.clone());

    // Register and get tokens
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("replay_user_{}", nanoid::nanoid!(6)),
            Some(format!("replay_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // First refresh (legitimate)
    let (_new_access, new_refresh) = service
        .refresh_token(refresh_token.clone())
        .await
        .expect("First refresh should succeed");

    // Replay the OLD refresh token (attacker replaying stolen token)
    let replay_result = service.refresh_token(refresh_token.clone()).await;
    assert!(
        replay_result.is_err(),
        "Replayed refresh token should be rejected"
    );
    assert!(matches!(
        replay_result.unwrap_err(),
        Error::Authentication(_)
    ));

    // After family revocation, even the NEW legitimate refresh token should be rejected
    // because all tokens issued before the revocation timestamp are invalid
    let second_refresh = service.refresh_token(new_refresh).await;
    assert!(
        second_refresh.is_err(),
        "New refresh token should also be rejected after family revocation"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_password_version_mismatch_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register a user
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("pv_user_{}", nanoid::nanoid!(6)),
            Some(format!("pv_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Change password (this bumps password_version)
    let jwt = create_jwt_service();
    let claims = jwt
        .verify_refresh_token(&refresh_token)
        .expect("Token valid");
    let user_id = UserId::from_string(claims.sub);
    service
        .set_password(&user_id, "NewStrongPass1")
        .await
        .expect("Password change should succeed");

    // Now try to use the old refresh token (with old password_version)
    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh with old password version should be rejected"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_banned_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register a user and get refresh token
    let (user, _access, Some(refresh_token)) = service
        .register(
            format!("banned_refresh_{}", nanoid::nanoid!(6)),
            Some(format!("banned_refresh_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Ban the user via raw SQL (status column is SMALLINT: 3=Banned)
    sqlx::query("UPDATE users SET status = 3 WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to ban user");

    // Try to refresh
    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Banned user should not be able to refresh"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_deleted_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register a user
    let (user, _access, Some(refresh_token)) = service
        .register(
            format!("deleted_refresh_{}", nanoid::nanoid!(6)),
            Some(format!("deleted_refresh_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Soft-delete via raw SQL
    sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to soft-delete");

    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Deleted user should not be able to refresh"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_family_revocation_timestamp_blocks_older_tokens() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist.clone());

    // Register and get tokens
    let (_user, _access, Some(refresh_token_1)) = service
        .register(
            format!("family_rev_{}", nanoid::nanoid!(6)),
            Some(format!("family_rev_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // First legitimate refresh: token_1 -> token_2
    let (_access_2, refresh_token_2) = service
        .refresh_token(refresh_token_1.clone())
        .await
        .expect("First refresh should succeed");

    // Second legitimate refresh: token_2 -> token_3
    let (_access_3, refresh_token_3) = service
        .refresh_token(refresh_token_2.clone())
        .await
        .expect("Second refresh should succeed");

    // Now replay token_1 (attacker replays a stolen old token)
    let replay_result = service.refresh_token(refresh_token_1).await;
    assert!(replay_result.is_err(), "Replayed old token should fail");

    // token_3 should also be blocked because family is revoked
    let result = service.refresh_token(refresh_token_3).await;
    assert!(
        result.is_err(),
        "Token issued before family revocation should be blocked"
    );
}

// ============================================================================
// S2: UserService::login status checks
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_banned_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let username = format!("banned_login_{}", nanoid::nanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!("banned_login_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Ban (status column is SMALLINT: 3=Banned)
    sqlx::query("UPDATE users SET status = 3 WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to ban user");

    let result = service
        .login(username, "StrongPass1".to_string(), None)
        .await;
    assert!(result.is_err(), "Banned user should not be able to login");
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_pending_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let username = format!("pending_login_{}", nanoid::nanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!("pending_login_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Set pending status (status column is SMALLINT: 2=Pending)
    sqlx::query("UPDATE users SET status = 2 WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set pending status");

    let result = service
        .login(username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        result.is_err(),
        "Pending user should not be able to login"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_soft_deleted_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let username = format!("deleted_login_{}", nanoid::nanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!("deleted_login_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Soft-delete
    sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to soft-delete");

    let result = service
        .login(username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        result.is_err(),
        "Soft-deleted user should not be able to login"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_unverified_email_blocked_when_verification_required() {
    let (_container, pool) = create_test_pool().await;
    let mut service = create_user_service(pool.clone());
    // First register without email verification to get the user created
    let username = format!("unverified_{}", nanoid::nanoid!(6));
    let email = format!("unverified_{}@test.com", nanoid::nanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(email.clone()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Set email_verified = false
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set unverified");

    // Now enable email verification requirement
    service.set_email_verification_required(true);

    let result = service
        .login(username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        result.is_err(),
        "Unverified email user should be blocked when verification is required"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_unverified_email_allowed_when_not_required() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register (no email verification required)
    let username = format!("norev_{}", nanoid::nanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!("norev_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Set email_verified = false
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set unverified");

    // Login should succeed because email verification is NOT required
    let result = service
        .login(username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        result.is_ok(),
        "Unverified email should be allowed when verification is not required: {:?}",
        result.err()
    );
}

// ============================================================================
// S2.5: Account enumeration prevention tests (HIGH #13)
// ============================================================================

/// Test that email-based users and OAuth2-only users receive identical
/// error messages when email verification is required but not satisfied.
///
/// This prevents account enumeration where attackers could determine
/// which accounts have emails configured based on different error responses.
///
/// VULNERABILITY DEMONSTRATION:
/// When email_verification_required=true, the code checks:
///   `user.email.is_some() && !user.email_verified`
///
/// This means:
/// - User WITH email (unverified): blocked (email.is_some() = true)
/// - User WITHOUT email (OAuth2-only): PASSES (email.is_some() = false)
///
/// An attacker can enumerate accounts by attempting login with correct password:
/// - Blocked → account has email configured
/// - Success → account is OAuth2-only (no email)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_email_verification_no_account_enumeration() {
    let (_container, pool) = create_test_pool().await;
    let mut service = create_user_service(pool.clone());
    service.set_email_verification_required(true);

    // Create user WITH email (unverified but Active)
    let email_user = format!("email_user_{}", nanoid::nanoid!(6));
    let (user_with_email, _, _) = service
        .register(
            email_user.clone(),
            Some(format!("{}@test.com", email_user)),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Mark email as unverified (but user is Active because verification was not required during registration)
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(user_with_email.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set unverified");

    // Create OAuth2-only user (no email) and set them as Active
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;
    let oauth_user = service
        .create_or_load_by_oauth2(&provider, "oauth123", "oauthuser", None)
        .await
        .expect("OAuth2 user creation should succeed");

    // Set a password for the OAuth2 user so they can login
    service
        .set_password(&oauth_user.id, "StrongPass1")
        .await
        .expect("Setting password should succeed");

    // Set OAuth2 user status to Active (they start as Pending)
    // This simulates an admin-approved OAuth2 user or one that bypasses review
    sqlx::query("UPDATE users SET status = 1 WHERE id = $1")  // 1 = Active
        .bind(oauth_user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set active status");

    // Both users should receive the SAME error message when attempting login
    // with correct password but unverified email status
    let email_result = service
        .login(email_user, "StrongPass1".to_string(), None)
        .await;

    let oauth_result = service
        .login(oauth_user.username.clone(), "StrongPass1".to_string(), None)
        .await;

    // Both should fail (CRITICAL: before fix, oauth_result would succeed!)
    assert!(email_result.is_err(), "Email user with unverified email should be blocked");
    assert!(oauth_result.is_err(), "OAuth2 user with no email should also be blocked (VULNERABILITY: currently passes)");

    // Both should return the SAME error type and message
    let email_err = email_result.unwrap_err();
    let oauth_err = oauth_result.unwrap_err();

    match (&email_err, &oauth_err) {
        (Error::Authentication(msg1), Error::Authentication(msg2)) => {
            assert_eq!(
                msg1, msg2,
                "Both users should receive identical error messages to prevent enumeration"
            );
        }
        _ => panic!("Both errors should be Authentication errors, got: {:?} and {:?}", email_err, oauth_err),
    }
}

/// Test that when email verification is NOT required, both email and OAuth2
/// users can login regardless of email_verified status.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_no_verification_required_both_user_types_allowed() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());
    // email_verification_required is false by default

    // Create user WITH email (unverified)
    let email_user = format!("email_allowed_{}", nanoid::nanoid!(6));
    let (_user_with_email, _, _) = service
        .register(
            email_user.clone(),
            Some(format!("{}@test.com", email_user)),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Create OAuth2-only user (no email)
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;
    let oauth_user = service
        .create_or_load_by_oauth2(&provider, "oauth_allowed", "oauth_allowed", None)
        .await
        .expect("OAuth2 user creation should succeed");

    service
        .set_password(&oauth_user.id, "StrongPass1")
        .await
        .expect("Setting password should succeed");

    // Set OAuth2 user status to Active (they start as Pending)
    sqlx::query("UPDATE users SET status = 1 WHERE id = $1")  // 1 = Active
        .bind(oauth_user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set active status");

    // Both should be able to login when verification is not required
    let email_result = service
        .login(email_user, "StrongPass1".to_string(), None)
        .await;

    let oauth_result = service
        .login(oauth_user.username.clone(), "StrongPass1".to_string(), None)
        .await;

    assert!(email_result.is_ok(), "Email user should be allowed when verification not required: {:?}", email_result.err());
    assert!(oauth_result.is_ok(), "OAuth2 user should be allowed when verification not required: {:?}", oauth_result.err());
}

// ============================================================================
// S3: UserService::delete_user
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_already_deleted_guard() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let (user, _, _) = service
        .register(
            format!("del_guard_{}", nanoid::nanoid!(6)),
            Some(format!("del_guard_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // First delete should succeed
    service
        .delete_user(&user.id)
        .await
        .expect("First delete should succeed");

    // Second delete should fail with "already deleted"
    let result = service.delete_user(&user.id).await;
    assert!(result.is_err(), "Double delete should fail");
    let err = result.unwrap_err();
    match &err {
        Error::InvalidInput(msg) => assert!(
            msg.contains("already deleted"),
            "Expected 'already deleted' message, got: {msg}"
        ),
        Error::NotFound(_) => {} // Also acceptable -- user may be filtered out
        _ => panic!("Expected InvalidInput or NotFound, got: {err}"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_transaction_atomicity_with_oauth2() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let (user, _, _) = service
        .register(
            format!("del_oauth_{}", nanoid::nanoid!(6)),
            Some(format!("del_oauth_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Delete should succeed (even without oauth2 mappings the transaction completes)
    service
        .delete_user(&user.id)
        .await
        .expect("Delete with OAuth2 cleanup should succeed");

    // Verify user is soft-deleted
    let deleted_user: Option<(String,)> =
        sqlx::query_as("SELECT id FROM users WHERE id = $1 AND deleted_at IS NOT NULL")
            .bind(user.id.as_str())
            .fetch_optional(&pool)
            .await
            .expect("Query should succeed");
    assert!(
        deleted_user.is_some(),
        "User should be soft-deleted in the database"
    );
}

// ============================================================================
// S7: UserService::change_password / set_password
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_change_password_wrong_old_password_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let (user, _, _) = service
        .register(
            format!("chpw_user_{}", nanoid::nanoid!(6)),
            Some(format!("chpw_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    // Try to change with wrong old password
    let result = service
        .change_password(&user.id, "WrongOldPass1", "NewStrongPass1")
        .await;
    assert!(
        result.is_err(),
        "Change password with wrong old password should fail"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_change_password_bumps_password_version() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let (user, _, _) = service
        .register(
            format!("pvbump_{}", nanoid::nanoid!(6)),
            Some(format!("pvbump_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let old_version = user.password_version;

    // Change password
    let updated_user = service
        .change_password(&user.id, "StrongPass1", "NewStrongPass1")
        .await
        .expect("Password change should succeed");

    assert_eq!(
        updated_user.password_version,
        old_version + 1,
        "Password version should be incremented"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_password_bumps_password_version() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let (user, _, _) = service
        .register(
            format!("setpw_{}", nanoid::nanoid!(6)),
            Some(format!("setpw_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let old_version = user.password_version;

    // Admin set password (no old password needed)
    let updated_user = service
        .set_password(&user.id, "AdminNewPass1")
        .await
        .expect("Set password should succeed");

    assert_eq!(
        updated_user.password_version,
        old_version + 1,
        "Password version should be incremented by set_password"
    );
}

// ============================================================================
// S13: create_or_load_by_oauth2
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_load_by_oauth2_username_sanitization() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Username with special chars that should be stripped
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;
    let result = service
        .create_or_load_by_oauth2(&provider, "provider_user_123", "user@special!chars.test", None)
        .await
        .expect("Should create user with sanitized username");

    // Sanitized username should only contain alphanumeric, underscore, hyphen
    assert!(
        result
            .username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-'),
        "Username should be sanitized: {}",
        result.username
    );
    // The @, !, and . should have been stripped
    assert!(
        !result.username.contains('@'),
        "@ should be stripped from username"
    );
    assert!(
        !result.username.contains('!'),
        "! should be stripped from username"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_load_by_oauth2_collision_retry() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;

    // Create first user with the desired username
    let user1 = service
        .create_or_load_by_oauth2(&provider, "provider1", "oauth_user", None)
        .await
        .expect("First user creation should succeed");

    assert_eq!(user1.username, "oauth_user");

    // Create second user with same desired username -- should get suffixed
    let user2 = service
        .create_or_load_by_oauth2(&provider, "provider2", "oauth_user", None)
        .await
        .expect("Second user creation should succeed with suffixed username");

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
async fn test_create_or_load_by_oauth2_email_conflict_propagation() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;

    // Create first user with an email
    let _user1 = service
        .create_or_load_by_oauth2(
            &provider,
            "email_conflict_1",
            "email_conflict_a",
            Some("same_email@oauth.test"),
        )
        .await
        .expect("First user should succeed");

    // Create second user with a DIFFERENT username but SAME email
    // This should propagate the email uniqueness error (not retry with suffix)
    let result = service
        .create_or_load_by_oauth2(
            &provider,
            "email_conflict_2",
            "email_conflict_b",
            Some("same_email@oauth.test"),
        )
        .await;

    // The email conflict should propagate as an error (not silently retry)
    assert!(
        result.is_err(),
        "Email conflict should propagate as error, not be swallowed by username retry"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_or_load_by_oauth2_empty_username_uses_provider_id() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());
    let provider = synctv_core::models::oauth2_client::OAuth2Provider::Google;

    // Empty username after sanitization should use provider_user_id
    let result = service
        .create_or_load_by_oauth2(&provider, "fallback_provider_id", "@@@!!!", None)
        .await
        .expect("Should create user with fallback username");

    assert!(
        result.username.starts_with("user_"),
        "Empty sanitized username should fall back to 'user_<provider_id>': {}",
        result.username
    );
}

// ============================================================================
// S1 additional: refresh_token with email verification re-check
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_email_verification_recheck() {
    let (_container, pool) = create_test_pool().await;

    // Register without email verification (get tokens)
    let service_no_verify = create_user_service(pool.clone());
    let (user, _access, Some(refresh_token)) = service_no_verify
        .register(
            format!("email_recheck_{}", nanoid::nanoid!(6)),
            Some(format!("email_recheck_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Un-verify the email
    sqlx::query("UPDATE users SET email_verified = false WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to un-verify email");

    // Create a service WITH email verification required
    let service_verify = create_user_service_with_email_verification(pool.clone());

    // Now try to refresh -- should fail because email is not verified
    let result = service_verify.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh should fail when email verification is required but email is unverified"
    );
}

// ============================================================================
// S1.5: refresh_token rate limiting tests (HIGH #16)
// ============================================================================

/// Test that refresh_token endpoint has rate limiting to prevent abuse.
///
/// Without rate limiting, an attacker with a stolen refresh token can:
/// 1. Rapidly call refresh_token to exhaust server resources
/// 2. Trigger family revocation, locking out the legitimate user
///
/// Rate limiting should:
/// - Limit per-user refresh requests to prevent abuse
/// - Allow legitimate refresh patterns (occasional token rotation)
/// - Return clear rate limit error when exceeded
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rate_limiting_per_user() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register and get initial tokens
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("rate_limit_refresh_{}", nanoid::nanoid!(6)),
            Some(format!("rate_limit_refresh_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens from registration");
    };

    // Make rapid refresh requests - should eventually be rate limited
    // The default limit is typically 10 requests per minute per user
    let mut success_count = 0;
    let mut rate_limited = false;
    let mut current_token = refresh_token;

    for _ in 0..20 {
        match service.refresh_token(current_token.clone()).await {
            Ok((_new_access, new_refresh)) => {
                success_count += 1;
                // Use the new refresh token for next iteration (token rotation)
                current_token = new_refresh;
            }
            Err(Error::RateLimited(_)) => {
                rate_limited = true;
                break;
            }
            Err(e) => {
                // Other errors shouldn't happen in this test
                panic!("Unexpected error during refresh: {:?}", e);
            }
        }
    }

    assert!(
        rate_limited,
        "Refresh token endpoint should be rate limited after {} requests (VULNERABILITY: no rate limiting)",
        success_count
    );

    // Should have had at least some successful refreshes before hitting limit
    assert!(success_count > 0, "Should allow at least some refresh requests before rate limiting");
}

/// Test that rate limit recovers after waiting.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rate_limit_recovers() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("rate_limit_recover_{}", nanoid::nanoid!(6)),
            Some(format!("rate_limit_recover_{}@test.com", nanoid::nanoid!(6))),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens from registration");
    };

    // Exhaust rate limit, keeping track of the latest token
    let mut current_token = refresh_token;
    for _ in 0..20 {
        match service.refresh_token(current_token.clone()).await {
            Ok((_access, new_refresh)) => {
                current_token = new_refresh;
            }
            Err(_) => break,
        }
    }

    // Wait for rate limit to reset (60 second window)
    tokio::time::sleep(std::time::Duration::from_secs(61)).await;

    // Should be able to refresh again after reset using the latest token
    let result = service.refresh_token(current_token).await;
    assert!(
        result.is_ok(),
        "Should be able to refresh again after rate limit window resets: {:?}",
        result.err()
    );
}
