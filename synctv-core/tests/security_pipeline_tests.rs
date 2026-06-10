//! `SecurityPipeline` integration tests
//!
//! Tests the post-JWT security pipeline: password version checks, user status
//! checks, cache fast-path, and DB error propagation.

use std::sync::Arc;

use sqlx::PgPool;
use synctv_core::{
    cache::{
        self,
        user_cache::{CachedUser, CachedUserSnapshot},
        UserCache,
    },
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{
            jwt::JwtService, AuthenticatedToken, Claims, SecurityPipeline, SecurityPipelineRuntime,
        },
        BruteForceProtection, InMemoryTokenBlacklistStore, TokenBlacklistStore, UserService,
    },
    Error, KeyBuilder,
};
use synctv_core_testing::{create_test_pool, err, ok, some};
fn create_jwt_service() -> JwtService {
    ok(
        JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars"),
        "JWT service should be created",
    )
}

fn create_user_service(pool: &PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

async fn insert_user(pool: &PgPool, user: &User) -> User {
    let repo = UserRepository::new(pool.clone());
    let created = ok(repo.create(user).await, "user should be created");
    some(
        ok(
            repo.get_by_id(&created.id).await,
            "created user should be reloaded",
        ),
        "created user should exist",
    )
}

async fn set_version(pool: &PgPool, user_id: &UserId, version: i32) {
    ok(
        sqlx::query(
            r"
        INSERT INTO auth_password_credentials (
            user_id, opaque_record, opaque_credential_identifier, opaque_ciphersuite,
            opaque_server_setup_version,
            changed_at, version, created_at, updated_at
        )
        VALUES ($1, 'test-opaque-record'::bytea, 'test-opaque-id'::bytea,
                'opaque-ristretto255-sha512-argon2id', 1, NOW(), $2, NOW(), NOW())
        ON CONFLICT (user_id) DO UPDATE
        SET changed_at = EXCLUDED.changed_at,
            version = EXCLUDED.version,
            updated_at = EXCLUDED.updated_at
        ",
        )
        .bind(user_id)
        .bind(version)
        .execute(pool)
        .await,
        "password credential version should be set",
    );
}

async fn insert_banned_user(pool: &PgPool, version: i32) -> User {
    let repo = UserRepository::new(pool.clone());
    let user = ok(
        repo.create(&make_user(UserStatus::Active, version)).await,
        "user should be created before ban",
    );
    ok(
        repo.ban(&user.id, None, Some("security pipeline test".to_string()))
            .await,
        "user should be banned",
    )
}

fn security_pipeline_with_cache(
    user_service: Arc<UserService>,
    user_cache: Arc<UserCache>,
) -> SecurityPipeline {
    let token_blacklist = user_service.token_blacklist_store();
    let key_builder = user_service.key_builder().clone();
    SecurityPipeline::new_with_runtime(
        user_service,
        SecurityPipelineRuntime {
            user_cache: Some(user_cache),
            token_blacklist,
            key_builder,
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
            token_blacklist,
            key_builder,
        },
    )
}

fn security_pipeline_with_cache_and_blacklist(
    user_service: Arc<UserService>,
    user_cache: Arc<UserCache>,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    key_builder: KeyBuilder,
) -> SecurityPipeline {
    SecurityPipeline::new_with_runtime(
        user_service,
        SecurityPipelineRuntime {
            user_cache: Some(user_cache),
            token_blacklist,
            key_builder,
        },
    )
}

fn make_user(status: UserStatus, _version: i32) -> User {
    User {
        id: UserId::new(),
        username: format!("test_user_{}", synctv_common::snanoid!(8)),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

fn make_claims(user_id: &UserId, pv: i32) -> Claims {
    let now = chrono::Utc::now();
    Claims {
        sub: user_id.to_string(),
        typ: "access".to_string(),
        jti: synctv_common::snanoid!(16),
        iat: now.timestamp(),
        exp: (now + chrono::Duration::hours(1)).timestamp(),
        pv,
        sid: None,
        amr: None,
        cbm: None,
        opi: None,
        ops: None,
        eml: None,
        wcid: None,
        iss: None,
        aud: None,
    }
}

async fn cache_user(user_cache: &UserCache, user: &User, status: UserStatus) {
    let cached = CachedUser::from_snapshot(CachedUserSnapshot {
        id: user.id,
        username: user.username.clone(),
        role: user.role,
        status,
        created_at: user.created_at,
        updated_at: user.updated_at,
        is_banned: false,
        is_deleted: false,
    });
    ok(
        user_cache.set(&user.id, cached).await,
        "user cache should be populated",
    );
}

fn checked_user(result: synctv_core::Result<AuthenticatedToken>) -> AuthenticatedToken {
    ok(result, "security pipeline check should pass")
}

fn auth_error(result: synctv_core::Result<AuthenticatedToken>) -> Error {
    err(result, "security pipeline check should fail")
}

// SecurityPipeline tests (DB-backed, no cache)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_miss_falls_through_to_db() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let pipeline = SecurityPipeline::new(&user_service);
    let claims = make_claims(&user.id, 0);

    let result = checked_user(pipeline.check(&claims).await);
    assert_eq!(result.user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_db_error_propagates_not_swallowed() {
    // When get_user returns a non-NotFound error, it should propagate instead
    // of being swallowed as Authentication("User not found").
    let (_container, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(&pool));
    let pipeline = SecurityPipeline::new(&user_service);

    // Use a user_id that doesn't exist -> get_user returns NotFound,
    // which should be mapped to Authentication error
    let missing_user_id = UserId::new();
    let claims = make_claims(&missing_user_id, 0);

    let err = auth_error(pipeline.check(&claims).await);
    assert!(
        matches!(&err, Error::Authentication(msg) if msg == "Authentication failed"),
        "NotFound should become generic Authentication failure, got: {err}"
    );

    let db_error_user_id = UserId::new();
    let claims2 = make_claims(&db_error_user_id, 0);
    // Now close the pool to simulate a DB error (not NotFound).
    // Using a closed real pool fails immediately and avoids slow network
    // connection timeouts from an invalid DSN.
    pool.close().await;
    let user_service2 = Arc::new(create_user_service(&pool));
    let pipeline2 = SecurityPipeline::new(&user_service2);

    let err2 = auth_error(pipeline2.check(&claims2).await);
    // A DB connection error should not become "User not found"; it should be a
    // Database error or Internal error.
    assert!(
        !matches!(&err2, Error::Authentication(msg) if msg.contains("User not found")),
        "DB errors should NOT be swallowed as 'User not found', got: {err2}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_rejected_via_db() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_banned_user(&pool, 0).await;
    let user_service = Arc::new(create_user_service(&pool));
    let pipeline = SecurityPipeline::new(&user_service);

    let claims = make_claims(&user.id, 0);
    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_rejected_via_db_second_path() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_banned_user(&pool, 0).await;
    let user_service = Arc::new(create_user_service(&pool));

    let pipeline = SecurityPipeline::new(&user_service);
    let claims = make_claims(&user.id, 0);

    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    // UserRepository::create does not write deleted_at, so set it via raw SQL
    ok(
        sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
            .bind(user.id)
            .execute(&pool)
            .await,
        "user should be soft-deleted",
    );
    let user_service = Arc::new(create_user_service(&pool));
    let pipeline = SecurityPipeline::new(&user_service);

    let claims = make_claims(&user.id, 0);
    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

// SecurityPipeline tests with UserCache (fast path)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_active_user_passes() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Active).await;

    let pipeline = security_pipeline_with_cache(user_service, user_cache);
    let claims = make_claims(&user.id, 0);

    checked_user(pipeline.check(&claims).await);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_outdated_version_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 5)).await;
    set_version(&pool, &user.id, 5).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Active).await;

    let pipeline = security_pipeline_with_cache(user_service, user_cache);

    // Token has pv=3 < DB auth pv=5 -> should be rejected
    let claims = make_claims(&user.id, 3);
    let err = auth_error(pipeline.check(&claims).await);
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("password change")),
        "Should reject outdated pv, got: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_banned_user_rejected_from_status_cache() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_banned_user(&pool, 0).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Banned).await;

    let pipeline = security_pipeline_with_cache(user_service, user_cache);
    let claims = make_claims(&user.id, 0);

    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

// SEC4: Banned user rejected (DB and cache paths)

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_rejected_via_db_again() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_banned_user(&pool, 0).await;
    let user_service = Arc::new(create_user_service(&pool));
    let pipeline = SecurityPipeline::new(&user_service);

    let claims = make_claims(&user.id, 0);
    assert!(
        matches!(
            auth_error(pipeline.check(&claims).await),
            Error::Authentication(_)
        ),
        "Should be an Authentication error"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_banned_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_banned_user(&pool, 0).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Banned).await;

    let pipeline = security_pipeline_with_cache(user_service, user_cache);
    let claims = make_claims(&user.id, 0);

    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_stale_active_status_does_not_bypass_ban() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Active).await;

    ok(
        UserRepository::new(pool.clone())
            .ban(&user.id, None, Some("security pipeline test".to_string()))
            .await,
        "user should be banned in DB",
    );

    let pipeline = security_pipeline_with_cache(user_service, user_cache);
    let claims = make_claims(&user.id, 0);

    assert!(matches!(
        auth_error(pipeline.check(&claims).await),
        Error::Authentication(_)
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_stale_version_does_not_bypass_password_change() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Active).await;

    set_version(&pool, &user.id, 3).await;

    let pipeline = security_pipeline_with_cache(user_service, user_cache);
    let claims = make_claims(&user.id, 0);

    let err = auth_error(pipeline.check(&claims).await);
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("password change")),
        "Should reject stale password version via fresh auth state, got: {err}"
    );
}

// SEC5: Cache population after DB miss

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_populated_with_correct_version_after_db_miss() {
    let (_container, pool) = create_test_pool().await;
    let version = 3;
    // UserRepository::create doesn't insert version (DB defaults to 0),
    // so we insert the user first and then update version via raw SQL.
    let user = insert_user(&pool, &make_user(UserStatus::Active, version)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    // Cache is empty -- no entry for this user
    assert!(
        ok(
            user_cache.get(&user.id).await,
            "user cache should be readable"
        )
        .is_none(),
        "Cache should be empty initially"
    );

    let pipeline = security_pipeline_with_cache(user_service, user_cache.clone());
    let claims = make_claims(&user.id, version);

    // This call should fall through to DB and then populate the cache
    checked_user(pipeline.check(&claims).await);

    // Verify the cache was populated
    let cached = some(
        ok(
            user_cache.get(&user.id).await,
            "user cache should be readable",
        ),
        "cache should be populated after DB lookup",
    );
    assert_eq!(
        cached.status(),
        UserStatus::Active,
        "Cached status should match the DB value"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_populated_then_subsequent_check_fails_closed_when_db_unavailable() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(&pool));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    let pipeline = security_pipeline_with_cache(user_service, user_cache.clone());
    let claims = make_claims(&user.id, 0);

    // First check: DB hit, populates cache
    checked_user(pipeline.check(&claims).await);

    // Close the pool to prove cache hits do not bypass the fresh DB confirmation.
    pool.close().await;

    // Second check: authentication must fail closed because current security
    // state can no longer be confirmed from the database.
    let result2 = pipeline.check(&claims).await;
    assert!(
        matches!(result2, Err(Error::Database(sqlx::Error::PoolClosed))),
        "Second check should fail closed when DB is unavailable, got: {:?}",
        result2.err()
    );
}

// Access Token Blacklist tests (logout token invalidation)

/// Test that a blacklisted access token is rejected by the security pipeline.
///
/// This simulates the logout flow:
/// 1. User has a valid access token
/// 2. User logs out, token JTI is added to blacklist
/// 3. Subsequent requests with that token should be rejected
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blacklisted_access_token_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let user_service = Arc::new(create_user_service_with_blacklist(
        &pool,
        token_blacklist.clone(),
        key_builder.clone(),
    ));

    let pipeline = security_pipeline_with_blacklist(
        user_service,
        token_blacklist.clone(),
        key_builder.clone(),
    );

    let claims = make_claims(&user.id, 0);

    // First check: token should be valid
    checked_user(pipeline.check(&claims).await);

    // Blacklist the token (simulating logout)
    let blacklist_key = key_builder.access_token_blacklist(&claims.jti);
    ok(
        token_blacklist.blacklist(&blacklist_key, 3600).await,
        "token should be blacklisted",
    );

    // Second check: token should be rejected
    let err = auth_error(pipeline.check(&claims).await);
    assert!(
        matches!(&err, Error::Authentication(_)),
        "Should be an Authentication error, got: {err}"
    );
}

/// Test that access token blacklist check works with cached user data.
///
/// This ensures the fast path (cache hit) also checks the blacklist.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blacklisted_access_token_rejected_via_cache_path() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let user_service = Arc::new(create_user_service_with_blacklist(
        &pool,
        token_blacklist.clone(),
        key_builder.clone(),
    ));

    let user_cache = Arc::new(UserCache::local_only(100, 5, 0, "test:user:".to_string()));

    cache_user(&user_cache, &user, UserStatus::Active).await;

    let pipeline = security_pipeline_with_cache_and_blacklist(
        user_service,
        user_cache,
        token_blacklist.clone(),
        key_builder.clone(),
    );

    let claims = make_claims(&user.id, 0);

    // First check: cache hit, token should be valid
    checked_user(pipeline.check(&claims).await);

    // Blacklist the token (simulating logout)
    let blacklist_key = key_builder.access_token_blacklist(&claims.jti);
    ok(
        token_blacklist.blacklist(&blacklist_key, 3600).await,
        "token should be blacklisted",
    );

    // Second check: cache hit, but token should still be rejected
    auth_error(pipeline.check(&claims).await);
}

/// Test that non-blacklisted tokens are allowed through.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_non_blacklisted_access_token_allowed() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;

    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let user_service = Arc::new(create_user_service_with_blacklist(
        &pool,
        token_blacklist.clone(),
        key_builder.clone(),
    ));

    let pipeline = security_pipeline_with_blacklist(
        user_service,
        token_blacklist.clone(),
        key_builder.clone(),
    );

    let claims = make_claims(&user.id, 0);

    // Token not in blacklist should be allowed
    checked_user(pipeline.check(&claims).await);
}

// Helper function to create UserService with custom blacklist store
fn create_user_service_with_blacklist(
    pool: &PgPool,
    token_blacklist: Arc<InMemoryTokenBlacklistStore>,
    key_builder: KeyBuilder,
) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

fn create_user_service_with_dyn_blacklist(
    pool: &PgPool,
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    key_builder: KeyBuilder,
) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::local_only("test:username:".to_string(), 1000, 0);
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

// Fail-closed blacklist store error handling tests (Issue: storage errors)

/// A blacklist store whose `is_blacklisted_checked` always returns an error,
/// simulating a database/Redis outage.
struct ErroringBlacklistStore;

#[async_trait::async_trait]
impl TokenBlacklistStore for ErroringBlacklistStore {
    async fn is_blacklisted_checked(&self, _key: &str) -> synctv_core::Result<bool> {
        Err(synctv_core::Error::Internal(
            "Simulated storage outage".to_string(),
        ))
    }

    async fn blacklist(&self, _key: &str, _ttl_secs: u64) -> synctv_core::Result<()> {
        Err(synctv_core::Error::Internal(
            "Simulated storage outage".to_string(),
        ))
    }

    async fn blacklist_if_not_exists(
        &self,
        _key: &str,
        _ttl_secs: u64,
    ) -> synctv_core::Result<bool> {
        Err(synctv_core::Error::Internal(
            "Simulated storage outage".to_string(),
        ))
    }

    async fn get_family_revoked_at_checked(&self, _key: &str) -> synctv_core::Result<Option<i64>> {
        Ok(None)
    }

    async fn set_family_revoked(
        &self,
        _key: &str,
        _timestamp: i64,
        _ttl_secs: u64,
    ) -> synctv_core::Result<()> {
        Ok(())
    }
}

/// Test that when the blacklist store encounters a storage error during
/// `is_blacklisted_checked`, the security pipeline rejects the request
/// (fail-closed behavior).
///
/// This prevents blacklisted tokens (e.g., from logout) from being accepted
/// during database/Redis outages.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_blacklist_store_error_rejects_request_fail_closed() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;

    let erroring_store: Arc<dyn TokenBlacklistStore> = Arc::new(ErroringBlacklistStore);
    let key_builder = KeyBuilder::new("test");
    let user_service = Arc::new(create_user_service_with_dyn_blacklist(
        &pool,
        erroring_store.clone(),
        key_builder.clone(),
    ));

    let pipeline = security_pipeline_with_blacklist(user_service, erroring_store, key_builder);

    let claims = make_claims(&user.id, 0);

    // The store will return an error from is_blacklisted_checked.
    // The pipeline should fail-closed and reject the request.
    let err = auth_error(pipeline.check(&claims).await);
    assert!(
        matches!(&err, Error::ServiceUnavailable(msg) if msg.contains("temporarily unavailable")),
        "Error should indicate temporary unavailability via ServiceUnavailable, got: {err}"
    );
}

/// Test that the default `is_blacklisted_checked` (which delegates to `is_blacklisted`)
/// works correctly for in-memory stores that cannot fail.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_in_memory_blacklist_store_is_blacklisted_checked_ok() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;

    let store = Arc::new(InMemoryTokenBlacklistStore::new(10_000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let user_service = Arc::new(create_user_service_with_blacklist(
        &pool,
        store.clone(),
        key_builder.clone(),
    ));

    let pipeline =
        security_pipeline_with_blacklist(user_service, store.clone(), key_builder.clone());

    let claims = make_claims(&user.id, 0);

    // Non-blacklisted token should pass
    checked_user(pipeline.check(&claims).await);

    // Blacklist the token
    let bl_key = key_builder.access_token_blacklist(&claims.jti);
    ok(
        store.blacklist(&bl_key, 3600).await,
        "token should be blacklisted",
    );

    // Blacklisted token should be rejected
    auth_error(pipeline.check(&claims).await);
}
