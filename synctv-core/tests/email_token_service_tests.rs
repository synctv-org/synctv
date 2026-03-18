//! `EmailTokenService` integration tests
//!
//! Tests the service-level token lifecycle: `generate_token`, `validate_token`,
//! expired token, wrong type, `invalidate_user_tokens`, concurrent single-use.
//!
//! Requires real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test --test `email_token_service_tests`
#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::{User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::email_token::{EmailTokenService, EmailTokenType},
};
use synctv_core_testing::create_test_pool;
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_generate_and_validate_lifecycle() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = user_repo.create(&make_user("svc_user_1")).await.unwrap();

    // Generate a token
    let token = service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert!(!token.is_empty());

    // Validate and consume the token
    let user_id = service
        .validate_token(&token, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert_eq!(user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_expired_token_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = synctv_core::repository::EmailTokenRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = user_repo.create(&make_user("svc_user_2")).await.unwrap();

    // Create a token that's already expired (bypass service to control expiry)
    let token_str = nanoid::nanoid!(64);
    let expired_at = Utc::now() - chrono::Duration::hours(1);
    token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::EmailVerification,
            expired_at,
        )
        .await
        .unwrap();

    // Service validation should fail
    let result = service
        .validate_token(&token_str, EmailTokenType::EmailVerification)
        .await;
    assert!(result.is_err(), "Expired token should fail validation");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_wrong_type_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = user_repo.create(&make_user("svc_user_3")).await.unwrap();

    // Generate an email verification token
    let token = service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Validate with wrong type should fail
    let result = service
        .validate_token(&token, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_err(), "Wrong token type should fail validation");

    // Correct type should still work
    let user_id = service
        .validate_token(&token, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert_eq!(user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_invalidate_user_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = user_repo.create(&make_user("svc_user_4")).await.unwrap();

    // Generate multiple tokens
    let token1 = service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    let token2 = service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Invalidate all email verification tokens for this user
    service
        .invalidate_user_tokens(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Both tokens should now fail
    assert!(
        service
            .validate_token(&token1, EmailTokenType::EmailVerification)
            .await
            .is_err(),
        "Token1 should be invalid after invalidate_user_tokens"
    );
    assert!(
        service
            .validate_token(&token2, EmailTokenType::EmailVerification)
            .await
            .is_err(),
        "Token2 should be invalid after invalidate_user_tokens"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_concurrent_single_use() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = user_repo.create(&make_user("svc_user_5")).await.unwrap();

    let token = service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Spawn 10 concurrent validation attempts
    let mut handles = Vec::new();
    for _ in 0..10 {
        let svc = service.clone();
        let t = token.clone();
        handles.push(tokio::spawn(async move {
            svc.validate_token(&t, EmailTokenType::PasswordReset).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.iter().filter(|r| r.is_err()).count();

    assert_eq!(
        successes, 1,
        "Exactly 1 concurrent validation should succeed (atomic single-use)"
    );
    assert_eq!(failures, 9, "9 concurrent validations should fail");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_validate_for_wrong_user_does_not_consume_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let owner = user_repo
        .create(&make_user("svc_user_owner"))
        .await
        .unwrap();
    let other = user_repo
        .create(&make_user("svc_user_other"))
        .await
        .unwrap();

    let token = service
        .generate_token(&owner.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    let wrong_user_attempt = service
        .validate_token_for_user(&token, EmailTokenType::PasswordReset, &other.id)
        .await;
    assert!(
        wrong_user_attempt.is_err(),
        "wrong-user validation must fail without consuming the token"
    );

    let owner_attempt = service
        .validate_token_for_user(&token, EmailTokenType::PasswordReset, &owner.id)
        .await
        .unwrap();
    assert_eq!(owner_attempt, owner.id);
}
