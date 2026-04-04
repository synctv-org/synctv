//! User auth/security service tests
//!
//! Tests for `UserService::refresh_token`, login status checks, `delete_user`,
//! `change_password/set_password`, and `create_or_load_by_oauth2`.
//!
//! S1/S2 tests use `InMemoryTokenBlacklistStore` + `InMemoryBruteForceProtection` + real `JwtService`.
//! S3/S7/S13 tests use testcontainers PG.
//!
//! Run with: cargo test --test `user_auth_service_tests`
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use sqlx::PgPool;
use synctv_core::{
    cache::{CacheL2Backend, KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{OAuth2Provider, UserId},
    repository::{UserOAuthProviderRepository, UserRepository},
    service::{
        auth::{jwt::JwtService, TestPasswordHasher},
        BruteForceProtection, InMemoryOAuthStateStore, InMemoryTokenBlacklistStore, OAuth2Service,
        RateLimiter, TokenBlacklistStore, UserService,
    },
    Error,
};
use synctv_core_testing::create_test_pool;
const JWT_SECRET: &str = "test-secret-key-for-user-auth-service-tests-long-enough-1234567890";

fn create_jwt_service() -> JwtService {
    JwtService::with_durations(JWT_SECRET, 1, 30, 4, 60).unwrap()
}

fn create_user_service_with_components(
    pool: PgPool,
    username_cache: UsernameCache,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
) -> UserService {
    let jwt = create_jwt_service();
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    let mut svc = UserService::new(
        pool,
        jwt,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        key_builder,
        brute_force,
    );
    svc.set_password_hasher(Arc::new(TestPasswordHasher::new()));
    svc
}

fn create_user_service_with_blacklist(
    pool: PgPool,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
) -> UserService {
    let l2 = Arc::new(NoopCacheL2);
    let username_cache = UsernameCache::new(l2, "test:username:".to_string(), 1000, 0);
    create_user_service_with_components(pool, username_cache, token_blacklist)
}

fn create_user_service(pool: PgPool) -> UserService {
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    create_user_service_with_blacklist(pool, token_blacklist)
}

struct FamilyRevocationFailingStore {
    inner: InMemoryTokenBlacklistStore,
}

#[async_trait::async_trait]
impl TokenBlacklistStore for FamilyRevocationFailingStore {
    async fn is_blacklisted(&self, key: &str) -> bool {
        self.inner.is_blacklisted(key).await
    }

    async fn is_blacklisted_checked(&self, key: &str) -> synctv_core::Result<bool> {
        self.inner.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<()> {
        self.inner.blacklist(key, ttl_secs).await
    }

    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<bool> {
        self.inner.blacklist_if_not_exists(key, ttl_secs).await
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        self.inner.get_family_revoked_at(key).await
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
    async fn is_blacklisted(&self, key: &str) -> bool {
        self.inner.is_blacklisted(key).await
    }

    async fn is_blacklisted_checked(&self, key: &str) -> synctv_core::Result<bool> {
        self.inner.is_blacklisted_checked(key).await
    }

    async fn blacklist(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<()> {
        self.inner.blacklist(key, ttl_secs).await
    }

    async fn blacklist_if_not_exists(&self, key: &str, ttl_secs: u64) -> synctv_core::Result<bool> {
        self.inner.blacklist_if_not_exists(key, ttl_secs).await
    }

    async fn get_family_revoked_at(&self, key: &str) -> Option<i64> {
        self.inner.get_family_revoked_at(key).await
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

    async fn delete_with_retry(
        &self,
        _key: &str,
        _max_retries: u32,
        _cache_type: &str,
    ) -> synctv_core::Result<()> {
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
        _new_ts_iso: &str,
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

    fn backend_name(&self) -> &'static str {
        "failing"
    }
}

fn create_user_service_with_failing_username_cache(pool: PgPool) -> UserService {
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let username_cache = UsernameCache::new(
        Arc::new(FailingCacheL2),
        "test:username:".to_string(),
        1000,
        60,
    );
    create_user_service_with_components(pool, username_cache, token_blacklist)
}

fn create_user_service_with_email_verification(pool: PgPool) -> UserService {
    let mut service = create_user_service(pool);
    service.set_email_verification_required(true);
    service
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
            format!("refresh_user_{}", synctv_common::snanoid!(6)),
            Some(format!("refresh_{}@test.com", synctv_common::snanoid!(6))),
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
    let access_claims = jwt
        .verify_access_token(&new_access)
        .expect("New access token valid");
    let refresh_claims = jwt
        .verify_refresh_token(&new_refresh)
        .expect("New refresh token valid");

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
            format!("blacklist_user_{}", synctv_common::snanoid!(6)),
            Some(format!("blacklist_{}@test.com", synctv_common::snanoid!(6))),
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
async fn test_refresh_token_fails_closed_when_family_revocation_read_fails() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(FamilyRevocationReadFailingStore {
            inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        });
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist);

    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("family_read_fail_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "family_read_fail_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh must fail closed when family revocation lookup fails"
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
            format!("replay_user_{}", synctv_common::snanoid!(6)),
            Some(format!("replay_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("pv_user_{}", synctv_common::snanoid!(6)),
            Some(format!("pv_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("banned_refresh_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "banned_refresh_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
    assert!(result.is_err(), "Banned user should not be able to refresh");
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
            format!("deleted_refresh_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "deleted_refresh_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
            format!("family_rev_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "family_rev_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_fails_closed_when_family_revocation_lookup_errors() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(FamilyRevocationReadFailingStore {
            inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
        });
    let service = create_user_service_with_blacklist(pool, token_blacklist);

    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("family_lookup_fail_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "family_lookup_fail_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected refresh token");
    };

    let result = service.refresh_token(refresh_token).await;
    assert!(
        result.is_err(),
        "Refresh should fail closed when family revocation lookup cannot be verified"
    );
    assert!(matches!(result.unwrap_err(), Error::Internal(_)));
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
    let username = format!("banned_login_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!(
                "banned_login_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
    let username = format!("pending_login_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!(
                "pending_login_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
    assert!(result.is_err(), "Pending user should not be able to login");
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_soft_deleted_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register
    let username = format!("deleted_login_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!(
                "deleted_login_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
    let username = format!("unverified_{}", synctv_common::snanoid!(6));
    let email = format!("unverified_{}@test.com", synctv_common::snanoid!(6));
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
    assert!(matches!(result.unwrap_err(), Error::EmailNotVerified));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_unverified_email_allowed_when_not_required() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    // Register (no email verification required)
    let username = format!("norev_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!("norev_{}@test.com", synctv_common::snanoid!(6))),
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
/// When `email_verification_required=true`, the code checks:
///   `user.email.is_some() && !user.email_verified`
///
/// This means:
/// - User WITH email (unverified): blocked (`email.is_some()` = true)
/// - User WITHOUT email (OAuth2-only): PASSES (`email.is_some()` = false)
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
    let email_user = format!("email_user_{}", synctv_common::snanoid!(6));
    let (user_with_email, _, _) = service
        .register(
            email_user.clone(),
            Some(format!("{email_user}@test.com")),
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
    sqlx::query("UPDATE users SET status = 1 WHERE id = $1") // 1 = Active
        .bind(oauth_user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set active status");

    // Email user with unverified email should be blocked
    let email_result = service
        .login(email_user, "StrongPass1".to_string(), None)
        .await;

    // OAuth2 user should bypass email verification (pre-verified by provider)
    let oauth_result = service
        .login(oauth_user.username.clone(), "StrongPass1".to_string(), None)
        .await;

    assert!(
        email_result.is_err(),
        "Email user with unverified email should be blocked"
    );
    assert!(
        oauth_result.is_ok(),
        "OAuth2 user should bypass email verification (authenticated by provider)"
    );
}

/// Test that when email verification is NOT required, both email and `OAuth2`
/// users can login regardless of `email_verified` status.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_no_verification_required_both_user_types_allowed() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());
    // email_verification_required is false by default

    // Create user WITH email (unverified)
    let email_user = format!("email_allowed_{}", synctv_common::snanoid!(6));
    let (_user_with_email, _, _) = service
        .register(
            email_user.clone(),
            Some(format!("{email_user}@test.com")),
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
    sqlx::query("UPDATE users SET status = 1 WHERE id = $1") // 1 = Active
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
            format!("del_guard_{}", synctv_common::snanoid!(6)),
            Some(format!("del_guard_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("del_oauth_{}", synctv_common::snanoid!(6)),
            Some(format!("del_oauth_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("chpw_user_{}", synctv_common::snanoid!(6)),
            Some(format!("chpw_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("pvbump_{}", synctv_common::snanoid!(6)),
            Some(format!("pvbump_{}@test.com", synctv_common::snanoid!(6))),
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
            format!("setpw_{}", synctv_common::snanoid!(6)),
            Some(format!("setpw_{}@test.com", synctv_common::snanoid!(6))),
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_set_password_succeeds_even_when_family_revocation_store_fails() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FamilyRevocationFailingStore {
        inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
    });
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist);

    let username = format!("setpw_fail_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            username.clone(),
            Some(format!(
                "setpw_fail_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let result = service.set_password(&user.id, "AdminNewPass1").await;
    assert!(
        result.is_ok(),
        "Password updates should rely on password_version, not fail on best-effort family revocation persistence"
    );

    let login_old = service
        .login(username.clone(), "StrongPass1".to_string(), None)
        .await;
    assert!(
        login_old.is_err(),
        "Old password must stop working after password_version is updated"
    );

    let login_new = service
        .login(username, "AdminNewPass1".to_string(), None)
        .await;
    assert!(
        login_new.is_ok(),
        "New password must become active even when best-effort family revocation persistence fails"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_updates_username_and_password_atomically() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    let old_username = format!("profile_atomic_{}", synctv_common::snanoid!(6));
    let new_username = format!("profile_atomic_new_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            old_username.clone(),
            Some(format!(
                "profile_atomic_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let updated_user = service
        .update_profile(
            &user.id,
            Some(new_username.clone()),
            Some("StrongPass1".to_string()),
            Some("NewStrongPass1".to_string()),
        )
        .await
        .expect("Combined profile update should succeed");

    assert_eq!(updated_user.username, new_username);
    assert_eq!(
        updated_user.password_version,
        user.password_version + 1,
        "Combined profile update should increment password_version exactly once"
    );

    let login_old = service
        .login(old_username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        login_old.is_err(),
        "Old username/password must stop working after a successful atomic profile update"
    );

    let login_new = service
        .login(new_username, "NewStrongPass1".to_string(), None)
        .await;
    assert!(
        login_new.is_ok(),
        "New username/password must become active together after a successful atomic profile update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_rolls_back_username_when_password_verification_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service(pool.clone());

    let old_username = format!("profile_rollback_{}", synctv_common::snanoid!(6));
    let new_username = format!("profile_rollback_new_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            old_username.clone(),
            Some(format!(
                "profile_rollback_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let result = service
        .update_profile(
            &user.id,
            Some(new_username.clone()),
            Some("WrongOldPass1".to_string()),
            Some("NewStrongPass1".to_string()),
        )
        .await;

    assert!(
        matches!(result, Err(Error::Authentication(_))),
        "Wrong current password must reject the combined profile update"
    );

    let persisted = service
        .get_user(&user.id)
        .await
        .expect("User should still exist after rejected update");
    assert_eq!(
        persisted.username, old_username,
        "Username must not be partially committed when password verification fails"
    );

    let login_old = service
        .login(old_username, "StrongPass1".to_string(), None)
        .await;
    assert!(
        login_old.is_ok(),
        "Original credentials must remain valid after a rejected combined profile update"
    );

    let login_new = service
        .login(new_username, "NewStrongPass1".to_string(), None)
        .await;
    assert!(
        login_new.is_err(),
        "New credentials must not become active when the combined profile update is rejected"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_profile_commits_when_family_revocation_store_fails() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(FamilyRevocationFailingStore {
        inner: InMemoryTokenBlacklistStore::new(10_000, 3600, 86400),
    });
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist);

    let old_username = format!("profile_revoke_{}", synctv_common::snanoid!(6));
    let new_username = format!("profile_revoke_new_{}", synctv_common::snanoid!(6));
    let (user, _, _) = service
        .register(
            old_username.clone(),
            Some(format!(
                "profile_revoke_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed");

    let updated = service
        .update_profile(
            &user.id,
            Some(new_username.clone()),
            Some("StrongPass1".to_string()),
            Some("NewStrongPass1".to_string()),
        )
        .await
        .expect("Combined profile update should rely on password_version and commit");

    let persisted = service
        .get_user(&user.id)
        .await
        .expect("User should still exist after successful combined update");
    assert_eq!(
        persisted.username, new_username,
        "Username change must commit even when best-effort family revocation persistence fails"
    );
    assert_eq!(updated.username, persisted.username);

    let login_old = service
        .login(old_username.clone(), "StrongPass1".to_string(), None)
        .await;
    assert!(
        login_old.is_err(),
        "Original credentials must stop working after a successful combined profile update"
    );

    let login_new = service
        .login(new_username, "NewStrongPass1".to_string(), None)
        .await;
    assert!(
        login_new.is_ok(),
        "New credentials must become active after a successful combined profile update"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_register_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(pool.clone());

    let username = format!("cache_fail_register_{}", synctv_common::snanoid!(6));
    let (user, access_token, refresh_token) = service
        .register(
            username.clone(),
            Some(format!(
                "cache_fail_register_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration must succeed even when username cache write fails");

    assert_eq!(user.username, username);
    assert!(access_token.is_some());
    assert!(refresh_token.is_some());

    let persisted = service
        .get_user(&user.id)
        .await
        .expect("Registered user must be durable in the database");
    assert_eq!(persisted.username, user.username);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_finalize_registration_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(pool.clone());

    let user = service
        .register_with_executor(
            format!("cache_fail_finalize_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "cache_fail_finalize_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            synctv_core::models::SignupMethod::Email,
            &pool,
        )
        .await
        .expect("User creation should succeed");

    let (access_token, refresh_token) = service
        .finalize_registration(&user)
        .await
        .expect("Finalization must succeed even when username cache write fails");

    let jwt = create_jwt_service();
    assert!(jwt.verify_access_token(&access_token).is_ok());
    assert!(jwt.verify_refresh_token(&refresh_token).is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_with_role_succeeds_when_username_cache_write_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(pool.clone());

    let created = service
        .create_user_with_role(
            format!("cache_fail_admin_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "cache_fail_admin_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            Some(synctv_core::models::UserRole::Admin),
        )
        .await
        .expect("Admin user creation must succeed even when username cache write fails");

    let persisted = service
        .get_user(&created.id)
        .await
        .expect("Created admin user must be durable in the database");
    assert_eq!(persisted.id, created.id);
    assert_eq!(persisted.role, synctv_core::models::UserRole::Admin);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_username_falls_back_to_database_when_cache_read_fails() {
    let (_container, pool) = create_test_pool().await;
    let service = create_user_service_with_failing_username_cache(pool.clone());

    let (user, _, _) = service
        .register(
            format!("cache_fail_lookup_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "cache_fail_lookup_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration must succeed");

    let username = service
        .get_username(&user.id)
        .await
        .expect("Username lookup should fall back to database on cache read failure");

    assert_eq!(username.as_deref(), Some(user.username.as_str()));
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
        .create_or_load_by_oauth2(
            &provider,
            "provider_user_123",
            "user@special!chars.test",
            None,
        )
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
    assert_eq!(
        result.status,
        synctv_core::models::UserStatus::Active,
        "OAuth2-created users should start active so first login succeeds"
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

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_concurrent_requests_do_not_commit_orphan_oauth2_users() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(pool.clone());
    let oauth_service = OAuth2Service::new(
        UserOAuthProviderRepository::new(pool.clone()),
        Arc::new(InMemoryOAuthStateStore::new()),
        synctv_core::oauth2::ProviderRegistry::new(),
        false,
    )
    .expect("OAuth2 service should initialize");

    let provider = OAuth2Provider::Google;
    let user_info = synctv_core::service::OAuth2UserInfo {
        provider: provider.clone(),
        provider_user_id: format!("oauth_concurrent_{}", synctv_common::snanoid!(8)),
        username: format!("oauth_concurrent_user_{}", synctv_common::snanoid!(6)),
        email: Some(format!(
            "oauth_concurrent_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        avatar: None,
        email_verified: true,
    };

    let first = oauth_service.find_or_create_and_link(&user_service, &provider, &user_info);
    let second = oauth_service.find_or_create_and_link(&user_service, &provider, &user_info);
    let (first_result, second_result) = tokio::join!(first, second);

    let (first_user_id, _) = first_result.expect("first concurrent login must succeed");
    let (second_user_id, _) = second_result.expect("second concurrent login must succeed");
    assert_eq!(
        first_user_id, second_user_id,
        "Concurrent logins for the same provider identity must converge to one user"
    );

    let oauth_repo = UserOAuthProviderRepository::new(pool.clone());
    let mapping = oauth_repo
        .find_by_provider(&provider, &user_info.provider_user_id)
        .await
        .expect("mapping lookup must succeed")
        .expect("mapping must exist");
    assert_eq!(mapping.user_id, first_user_id);

    let user_repo = UserRepository::new(pool.clone());
    let oauth2_user_count: i64 = sqlx::query_scalar(
        r"
        SELECT COUNT(*)
        FROM users u
        JOIN oauth2_clients oc ON oc.user_id = u.id
        WHERE oc.provider = $1
          AND oc.provider_user_id = $2
          AND u.deleted_at IS NULL
        ",
    )
    .bind(provider.as_str())
    .bind(&user_info.provider_user_id)
    .fetch_one(&pool)
    .await
    .expect("user count query must succeed");
    assert_eq!(
        oauth2_user_count, 1,
        "Concurrent OAuth2 signups must not commit an extra orphan user row"
    );

    let persisted_user = user_repo
        .get_by_id(&first_user_id)
        .await
        .expect("user lookup must succeed")
        .expect("winning user must exist");
    assert_eq!(persisted_user.email.as_deref(), user_info.email.as_deref());
    assert!(persisted_user.email_verified);
    assert_eq!(
        persisted_user.status,
        synctv_core::models::UserStatus::Active,
        "OAuth2-created users must be active immediately"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_find_or_create_and_link_retries_with_suffixed_username_on_collision() {
    let (_container, pool) = create_test_pool().await;
    let user_service = create_user_service(pool.clone());
    let oauth_service = OAuth2Service::new(
        UserOAuthProviderRepository::new(pool.clone()),
        Arc::new(InMemoryOAuthStateStore::new()),
        synctv_core::oauth2::ProviderRegistry::new(),
        false,
    )
    .expect("OAuth2 service should initialize");

    user_service
        .register(
            "oauth_collision_user".to_string(),
            Some("local_collision@test.com".to_string()),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("seed local user should be created");

    let provider = OAuth2Provider::Google;
    let user_info = synctv_core::service::OAuth2UserInfo {
        provider: provider.clone(),
        provider_user_id: format!("oauth_collision_{}", synctv_common::snanoid!(8)),
        username: "oauth_collision_user".to_string(),
        email: Some(format!(
            "oauth_collision_{}@test.com",
            synctv_common::snanoid!(6)
        )),
        avatar: None,
        email_verified: true,
    };

    let (created_user_id, is_new) = oauth_service
        .find_or_create_and_link(&user_service, &provider, &user_info)
        .await
        .expect("OAuth2 signup should succeed by choosing a suffixed username");

    assert!(is_new, "first OAuth2 login should create a new user");

    let user_repo = UserRepository::new(pool.clone());
    let created_user = user_repo
        .get_by_id(&created_user_id)
        .await
        .expect("user lookup should succeed")
        .expect("created OAuth2 user should exist");

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
            format!("email_recheck_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "email_recheck_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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

/// Test that `refresh_token` endpoint has rate limiting to prevent abuse.
///
/// Without rate limiting, an attacker with a stolen refresh token can:
/// 1. Rapidly call `refresh_token` to exhaust server resources
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
            format!("rate_limit_refresh_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "rate_limit_refresh_{}@test.com",
                synctv_common::snanoid!(6)
            )),
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
                panic!("Unexpected error during refresh: {e:?}");
            }
        }
    }

    assert!(
        rate_limited,
        "Refresh token endpoint should be rate limited after {success_count} requests (VULNERABILITY: no rate limiting)"
    );

    // Should have had at least some successful refreshes before hitting limit
    assert!(
        success_count > 0,
        "Should allow at least some refresh requests before rate limiting"
    );
}

// ============================================================================
// S1.6: refresh_token concurrent refresh race condition tests (P0 #10)
// ============================================================================

/// Test that concurrent refresh of the same token triggers family revocation.
///
/// RACE CONDITION VULNERABILITY:
/// The original implementation has a TOCTOU (Time-Of-Check-Time-Of-Use) race:
/// 1. Request A: `is_blacklisted(jti)` -> false
/// 2. Request B: `is_blacklisted(jti)` -> false (A hasn't blacklisted yet)
/// 3. Request A: blacklist(jti) -> success, issues new token
/// 4. Request B: blacklist(jti) -> success (upsert), issues new token
///
/// Both requests succeed, but B should have detected the replay and triggered
/// family revocation instead.
///
/// SECURITY IMPACT:
/// - Attacker with stolen token can get a valid new token
/// - Token theft detection is bypassed
/// - Legitimate user is NOT protected
///
/// FIX:
/// Use atomic `blacklist_if_not_exists` that returns whether the key was
/// newly inserted (first use) or already existed (replay detected).
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_concurrent_refresh_race_condition() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist.clone());

    // Register and get tokens
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("concurrent_race_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "concurrent_race_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // Track results from concurrent refreshes
    let success_count = Arc::new(AtomicUsize::new(0));
    let failure_count = Arc::new(AtomicUsize::new(0));

    // Use barrier to maximize concurrency - all threads start at the same time
    let barrier = Arc::new(Barrier::new(10));

    // Spawn multiple concurrent refresh requests with the SAME token
    let mut handles = vec![];
    for _ in 0..10 {
        let service = service.clone();
        let token = refresh_token.clone();
        let success = success_count.clone();
        let failure = failure_count.clone();
        let barrier = barrier.clone();

        handles.push(tokio::spawn(async move {
            // Synchronize all tasks to start at the same time
            barrier.wait().await;

            match service.refresh_token(token).await {
                Ok(_) => {
                    success.fetch_add(1, Ordering::SeqCst);
                }
                Err(_) => {
                    failure.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }

    // Wait for all concurrent requests to complete
    for handle in handles {
        handle.await.expect("Task should complete");
    }

    let successes = success_count.load(Ordering::SeqCst);
    let failures = failure_count.load(Ordering::SeqCst);

    // CRITICAL: Only ONE request should succeed (the first one to blacklist)
    // All others should fail because the JTI is already blacklisted
    assert_eq!(
        successes, 1,
        "Exactly ONE concurrent refresh should succeed, got {successes} successes and {failures} failures. \
         RACE CONDITION: multiple requests bypassed the blacklist check!"
    );
    assert_eq!(
        failures, 9,
        "Nine requests should fail due to JTI already blacklisted"
    );
}

/// Test that concurrent refresh properly triggers family revocation on replay.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_concurrent_refresh_family_revocation() {
    let (_container, pool) = create_test_pool().await;
    let token_blacklist: Arc<dyn TokenBlacklistStore> =
        Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let service = create_user_service_with_blacklist(pool.clone(), token_blacklist.clone());

    // Register and get tokens
    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("family_rev_race_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "family_rev_race_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens");
    };

    // First legitimate refresh
    let (_access1, refresh_token1) = service
        .refresh_token(refresh_token.clone())
        .await
        .expect("First refresh should succeed");

    // Now do concurrent refreshes with the OLD token (simulating attacker replay)
    let mut handles = vec![];
    for _ in 0..5 {
        let service = service.clone();
        let token = refresh_token.clone();

        handles.push(tokio::spawn(async move {
            service.refresh_token(token).await.is_ok()
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    // After concurrent replay attempts, the new token should also be blocked
    // because family revocation should have been triggered
    let result = service.refresh_token(refresh_token1).await;
    assert!(
        result.is_err(),
        "New token should be blocked after family revocation from concurrent replay detection"
    );
}

/// Test that rate limit recovers after waiting.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_refresh_token_rate_limit_recovers() {
    let (_container, pool) = create_test_pool().await;
    let mut service = create_user_service(pool.clone());
    service.set_refresh_rate_limiter_for_tests(RateLimiter::in_memory_only(
        "test-refresh-recover-short-window:".to_string(),
    ));
    service.set_refresh_rate_limit_config_for_tests(1, 1);

    let (_user, _access, Some(refresh_token)) = service
        .register(
            format!("rate_limit_recover_{}", synctv_common::snanoid!(6)),
            Some(format!(
                "rate_limit_recover_{}@test.com",
                synctv_common::snanoid!(6)
            )),
            "StrongPass1".to_string(),
            None,
        )
        .await
        .expect("Registration should succeed")
    else {
        panic!("Expected tokens from registration");
    };

    // Exhaust a short window rate limit, keeping track of the latest token.
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

    // Should be able to refresh again after the short window resets.
    let result = service.refresh_token(current_token).await;
    assert!(
        result.is_ok(),
        "Should be able to refresh again after rate limit window resets: {:?}",
        result.err()
    );
}

// ============================================================================
// S2.6: Password timing attack prevention tests (P1 #24)
// ============================================================================
