//! SecurityPipeline integration tests
//!
//! Tests the post-JWT security pipeline: password version checks, user status
//! checks, cache fast-path, and the B3 bug fix (DB error propagation).
//!
//! Run with: cargo test --test security_pipeline_tests -- --nocapture

use std::sync::Arc;

use synctv_core::{
    cache::{self, user_cache::CachedUser, NoopCacheL2, UserCache},
    config::PasswordComplexityConfig,
    models::{User, UserId, UserRole, UserStatus, SignupMethod},
    repository::UserRepository,
    service::{
        auth::{jwt::JwtService, Claims, SecurityPipeline},
        BruteForceProtection, InMemoryTokenBlacklistStore, UserService,
    },
    Error, KeyBuilder,
};
use sqlx::PgPool;
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "16-alpine";

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

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

fn create_user_service(pool: PgPool) -> UserService {
    let jwt_service = create_jwt_service();
    let username_cache = cache::UsernameCache::new(
        Arc::new(NoopCacheL2),
        "test:username:".to_string(),
        1000,
        0,
    );
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

async fn insert_user(pool: &PgPool, user: &User) -> User {
    let repo = UserRepository::new(pool.clone());
    repo.create(user).await.expect("Failed to create user")
}

fn make_user(status: UserStatus, password_version: i32) -> User {
    User {
        id: UserId::new(),
        username: format!("test_user_{}", nanoid::nanoid!(8)),
        email: Some(format!("{}@test.com", nanoid::nanoid!(8))),
        password_hash: "$argon2id$v=19$m=16384,t=3,p=1$fake$fakehash".to_string(),
        role: UserRole::User,
        status,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version,
        version: 0,
        deleted_at: None,
    }
}

fn make_claims(user_id: &UserId, pv: Option<i32>) -> Claims {
    let now = chrono::Utc::now();
    Claims {
        sub: user_id.as_str().to_string(),
        typ: "access".to_string(),
        jti: nanoid::nanoid!(16),
        iat: now.timestamp(),
        exp: (now + chrono::Duration::hours(1)).timestamp(),
        pv,
        iss: None,
        aud: None,
    }
}

// ============================================================================
// SecurityPipeline tests (DB-backed, no cache)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_miss_falls_through_to_db() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(pool));

    let pipeline = SecurityPipeline::new(user_service);
    let claims = make_claims(&user.id, Some(0));

    let result = pipeline.check(&claims).await;
    assert!(result.is_ok(), "Active user with valid pv should pass");
    assert_eq!(result.unwrap().user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_db_error_propagates_not_swallowed() {
    // B3 fix: When get_user returns a non-NotFound error, it should propagate
    // instead of being swallowed as Authentication("User not found").
    let (_container, pool) = create_test_pool().await;
    let user_service = Arc::new(create_user_service(pool.clone()));
    let pipeline = SecurityPipeline::new(user_service);

    // Use a user_id that doesn't exist -> get_user returns NotFound,
    // which should be mapped to Authentication error
    let fake_id = UserId::new();
    let claims = make_claims(&fake_id, Some(0));

    let result = pipeline.check(&claims).await;
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("User not found")),
        "NotFound should become Authentication('User not found'), got: {err}"
    );

    // Now close the pool to simulate a DB error (not NotFound)
    pool.close().await;

    let fake_id2 = UserId::new();
    let claims2 = make_claims(&fake_id2, Some(0));
    let user_service2 = Arc::new(create_user_service(
        PgPool::connect_lazy("postgresql://invalid:invalid@127.0.0.1:1/invalid").unwrap(),
    ));
    let pipeline2 = SecurityPipeline::new(user_service2);

    let result2 = pipeline2.check(&claims2).await;
    assert!(result2.is_err());

    let err2 = result2.unwrap_err();
    // With B3 fix, a DB connection error should NOT become "User not found"
    // It should be a Database error or Internal error
    assert!(
        !matches!(&err2, Error::Authentication(msg) if msg.contains("User not found")),
        "DB errors should NOT be swallowed as 'User not found', got: {err2}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_rejected_via_db() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Banned, 0)).await;
    let user_service = Arc::new(create_user_service(pool));
    let pipeline = SecurityPipeline::new(user_service);

    let claims = make_claims(&user.id, Some(0));
    let result = pipeline.check(&claims).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_deleted_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    // UserRepository::create does not write deleted_at, so set it via raw SQL
    sqlx::query("UPDATE users SET deleted_at = NOW() WHERE id = $1")
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to soft-delete user");
    let user_service = Arc::new(create_user_service(pool));
    let pipeline = SecurityPipeline::new(user_service);

    let claims = make_claims(&user.id, Some(0));
    let result = pipeline.check(&claims).await;
    assert!(result.is_err(), "Deleted user should be rejected");
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_legacy_token_iat_before_password_change_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = make_user(UserStatus::Active, 1);
    // password_changed_at is "now"
    let user = insert_user(&pool, &user).await;
    let user_service = Arc::new(create_user_service(pool));
    let pipeline = SecurityPipeline::new(user_service);

    // Legacy token: no pv, iat BEFORE password_changed_at
    let mut claims = make_claims(&user.id, None);
    claims.iat = (chrono::Utc::now() - chrono::Duration::hours(2)).timestamp();

    let result = pipeline.check(&claims).await;
    assert!(result.is_err(), "Legacy token with old iat should be rejected");
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("password change")),
        "Should mention password change, got: {err}"
    );
}

// ============================================================================
// SecurityPipeline tests with UserCache (fast path)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_active_user_passes() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(pool));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    // Pre-populate cache
    let cached = CachedUser::with_updated_at(
        user.id.as_str().to_string(),
        user.username.clone(),
        user.role,
        UserStatus::Active,
        user.created_at,
        user.updated_at,
        0,
        false,
    );
    user_cache.set(&user.id, cached).await.unwrap();

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache);
    let claims = make_claims(&user.id, Some(0));

    let result = pipeline.check(&claims).await;
    assert!(result.is_ok(), "Cached active user should pass");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_outdated_password_version_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 5)).await;
    let user_service = Arc::new(create_user_service(pool));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    // Cache with password_version=5
    let cached = CachedUser::with_updated_at(
        user.id.as_str().to_string(),
        user.username.clone(),
        user.role,
        UserStatus::Active,
        user.created_at,
        user.updated_at,
        5,
        false,
    );
    user_cache.set(&user.id, cached).await.unwrap();

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache);

    // Token has pv=3 < cached pv=5 -> should be rejected
    let claims = make_claims(&user.id, Some(3));
    let result = pipeline.check(&claims).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(&err, Error::Authentication(msg) if msg.contains("password change")),
        "Should reject outdated pv, got: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_banned_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Banned, 0)).await;
    let user_service = Arc::new(create_user_service(pool));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    let cached = CachedUser::with_updated_at(
        user.id.as_str().to_string(),
        user.username.clone(),
        user.role,
        UserStatus::Banned,
        user.created_at,
        user.updated_at,
        0,
        false,
    );
    user_cache.set(&user.id, cached).await.unwrap();

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache);
    let claims = make_claims(&user.id, Some(0));

    let result = pipeline.check(&claims).await;
    assert!(result.is_err(), "Banned user should be rejected via cache");
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

// ============================================================================
// SEC4: Pending user rejected (DB and cache paths)
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pending_user_rejected_via_db() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Pending, 0)).await;
    let user_service = Arc::new(create_user_service(pool));
    let pipeline = SecurityPipeline::new(user_service);

    let claims = make_claims(&user.id, Some(0));
    let result = pipeline.check(&claims).await;
    assert!(result.is_err(), "Pending user should be rejected via DB");
    assert!(
        matches!(result.unwrap_err(), Error::Authentication(_)),
        "Should be an Authentication error"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_hit_pending_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Pending, 0)).await;
    let user_service = Arc::new(create_user_service(pool));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    // Pre-populate cache with Pending status
    let cached = CachedUser::with_updated_at(
        user.id.as_str().to_string(),
        user.username.clone(),
        user.role,
        UserStatus::Pending,
        user.created_at,
        user.updated_at,
        0,
        false,
    );
    user_cache.set(&user.id, cached).await.unwrap();

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache);
    let claims = make_claims(&user.id, Some(0));

    let result = pipeline.check(&claims).await;
    assert!(
        result.is_err(),
        "Pending user should be rejected via cache fast path"
    );
    assert!(matches!(result.unwrap_err(), Error::Authentication(_)));
}

// ============================================================================
// SEC5: Cache population after DB miss
// ============================================================================

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_populated_with_correct_password_version_after_db_miss() {
    let (_container, pool) = create_test_pool().await;
    let password_version = 3;
    // UserRepository::create doesn't insert password_version (DB defaults to 0),
    // so we insert the user first and then update password_version via raw SQL.
    let user = insert_user(&pool, &make_user(UserStatus::Active, password_version)).await;
    sqlx::query("UPDATE users SET password_version = $1 WHERE id = $2")
        .bind(password_version)
        .bind(user.id.as_str())
        .execute(&pool)
        .await
        .expect("Failed to set password_version");
    let user_service = Arc::new(create_user_service(pool));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    // Cache is empty -- no entry for this user
    assert!(
        user_cache.get(&user.id).await.unwrap().is_none(),
        "Cache should be empty initially"
    );

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache.clone());
    let claims = make_claims(&user.id, Some(password_version));

    // This call should fall through to DB and then populate the cache
    let result = pipeline.check(&claims).await;
    assert!(result.is_ok(), "Active user should pass: {:?}", result.err());

    // Verify the cache was populated
    let cached = user_cache
        .get(&user.id)
        .await
        .unwrap()
        .expect("Cache should be populated after DB lookup");
    assert_eq!(
        cached.password_version(),
        password_version,
        "Cached password_version should match the DB value"
    );
    assert_eq!(
        cached.status(),
        UserStatus::Active,
        "Cached status should match the DB value"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cache_populated_then_subsequent_check_uses_cache() {
    let (_container, pool) = create_test_pool().await;
    let user = insert_user(&pool, &make_user(UserStatus::Active, 0)).await;
    let user_service = Arc::new(create_user_service(pool.clone()));

    let user_cache = Arc::new(
        UserCache::new(Arc::new(NoopCacheL2), 100, 5, 0, "test:user:".to_string()).unwrap(),
    );

    let pipeline = SecurityPipeline::new(user_service).with_user_cache(user_cache.clone());
    let claims = make_claims(&user.id, Some(0));

    // First check: DB hit, populates cache
    let result1 = pipeline.check(&claims).await;
    assert!(result1.is_ok());

    // Close the pool to prove the second check uses the cache, not DB
    pool.close().await;

    // Second check: should succeed from cache even though DB is closed
    let result2 = pipeline.check(&claims).await;
    assert!(
        result2.is_ok(),
        "Second check should succeed from cache even with DB closed: {:?}",
        result2.err()
    );
}
