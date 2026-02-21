//! Email token lifecycle integration tests
//!
//! Tests token creation, usage (mark as used), expiry checking, and cleanup.
//! Requires real PostgreSQL via testcontainers.
//!
//! Run with: cargo test --test email_token_tests

use synctv_core::{
    models::{UserId, User, UserRole, UserStatus},
    repository::{UserRepository, EmailTokenRepository},
    service::email_token::EmailTokenType,
};
use chrono::{Utc, Duration};
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

async fn create_test_pool() -> (ContainerAsync<Postgres>, PgPool) {
    let postgres = Postgres::default()
        .with_db_name("synctv_test")
        .with_user("synctv")
        .with_password("synctv_test")
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_test@127.0.0.1:{}/synctv_test",
        postgres.get_host_port_ipv4(5432).await.expect("Failed to get port")
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

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{}@test.com", username)),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: None,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

#[tokio::test]
async fn test_create_and_get_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_1")).await.unwrap();

    let token_str = nanoid::nanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    let created = token_repo.create(
        &token_str,
        &user.id,
        EmailTokenType::EmailVerification,
        expires_at,
    ).await.unwrap();

    assert_eq!(created.token, token_str);
    assert_eq!(created.user_id, user.id);
    assert_eq!(created.token_type, "email_verification");
    assert!(created.used_at.is_none());

    // Get the token
    let fetched = token_repo.get(&token_str).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_eq!(fetched.token, token_str);
    assert_eq!(fetched.user_id, user.id);
}

#[tokio::test]
async fn test_mark_token_as_used() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_2")).await.unwrap();

    let token_str = nanoid::nanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo.create(
        &token_str,
        &user.id,
        EmailTokenType::EmailVerification,
        expires_at,
    ).await.unwrap();

    // Mark as used
    let used = token_repo.mark_as_used(&token_str).await.unwrap();
    assert!(used.used_at.is_some(), "Token should have used_at set");

    // Verify it's marked as used when fetched
    let fetched = token_repo.get(&token_str).await.unwrap().unwrap();
    assert!(fetched.used_at.is_some());
}

#[tokio::test]
async fn test_validate_and_consume_valid_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_3")).await.unwrap();

    let token_str = nanoid::nanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo.create(
        &token_str,
        &user.id,
        EmailTokenType::PasswordReset,
        expires_at,
    ).await.unwrap();

    // Validate and consume
    let consumed = token_repo.validate_and_consume(&token_str, EmailTokenType::PasswordReset).await.unwrap();
    assert!(consumed.is_some(), "Valid token should be consumed");
    assert!(consumed.unwrap().used_at.is_some());

    // Consuming again should return None (already used)
    let consumed_again = token_repo.validate_and_consume(&token_str, EmailTokenType::PasswordReset).await.unwrap();
    assert!(consumed_again.is_none(), "Already-used token should not be consumable");
}

#[tokio::test]
async fn test_validate_wrong_type_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_4")).await.unwrap();

    let token_str = nanoid::nanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    // Create as EmailVerification
    token_repo.create(
        &token_str,
        &user.id,
        EmailTokenType::EmailVerification,
        expires_at,
    ).await.unwrap();

    // Try to consume as PasswordReset - wrong type
    let result = token_repo.validate_and_consume(&token_str, EmailTokenType::PasswordReset).await.unwrap();
    assert!(result.is_none(), "Token with wrong type should not be consumed");

    // The original type should still work
    let result = token_repo.validate_and_consume(&token_str, EmailTokenType::EmailVerification).await.unwrap();
    assert!(result.is_some());
}

#[tokio::test]
async fn test_expired_token_not_consumable() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_5")).await.unwrap();

    let token_str = nanoid::nanoid!(32);
    // Set expiry in the past
    let expires_at = Utc::now() - Duration::hours(1);

    token_repo.create(
        &token_str,
        &user.id,
        EmailTokenType::EmailVerification,
        expires_at,
    ).await.unwrap();

    // Should not be consumable (expired)
    let result = token_repo.validate_and_consume(&token_str, EmailTokenType::EmailVerification).await.unwrap();
    assert!(result.is_none(), "Expired token should not be consumable");
}

#[tokio::test]
async fn test_cleanup_expired_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_6")).await.unwrap();

    // Create some expired tokens
    for i in 0..3 {
        let token_str = nanoid::nanoid!(32);
        let expires_at = Utc::now() - Duration::hours(i + 1);
        token_repo.create(&token_str, &user.id, EmailTokenType::EmailVerification, expires_at).await.unwrap();
    }

    // Create one valid token
    let valid_token = nanoid::nanoid!(32);
    let valid_expires = Utc::now() + Duration::hours(24);
    token_repo.create(&valid_token, &user.id, EmailTokenType::EmailVerification, valid_expires).await.unwrap();

    // Cleanup expired
    let cleaned = token_repo.cleanup_expired().await.unwrap();
    assert_eq!(cleaned, 3, "Should clean up 3 expired tokens");

    // Valid token should still exist
    let fetched = token_repo.get(&valid_token).await.unwrap();
    assert!(fetched.is_some(), "Valid token should survive cleanup");
}

#[tokio::test]
async fn test_delete_user_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_7")).await.unwrap();

    // Create multiple unused tokens
    for _ in 0..3 {
        let token_str = nanoid::nanoid!(32);
        let expires_at = Utc::now() + Duration::hours(24);
        token_repo.create(&token_str, &user.id, EmailTokenType::EmailVerification, expires_at).await.unwrap();
    }

    // Delete all verification tokens for user
    let deleted = token_repo.delete_user_tokens(&user.id, EmailTokenType::EmailVerification).await.unwrap();
    assert_eq!(deleted, 3);
}

#[tokio::test]
async fn test_get_nonexistent_token() {
    let (_container, pool) = create_test_pool().await;
    let token_repo = EmailTokenRepository::new(pool.clone());

    let result = token_repo.get("nonexistent_token_value").await.unwrap();
    assert!(result.is_none());
}
