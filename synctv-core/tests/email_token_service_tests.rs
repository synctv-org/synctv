//! `EmailTokenService` integration tests
//!
//! Tests the service-level token lifecycle: `generate_token`, `validate_token`,
//! expired token, wrong type, `invalidate_user_tokens`, concurrent single-use.
//!
//! Requires real `PostgreSQL` via testcontainers.
//!

use chrono::Utc;
use futures::future::join_all;
use synctv_core::{
    models::{EmailTokenType, User, UserId, UserRole, UserStatus},
    repository::{EmailTokenRepository, UserRepository},
    service::EmailTokenService,
};
use synctv_core_testing::{create_test_pool, ok};
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_generate_and_validate_lifecycle() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = ok(
        user_repo.create(&make_user("svc_user_1")).await,
        "email token lifecycle user should be created",
    );

    // Generate a token
    let token = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailBind)
            .await,
        "email bind token should be generated",
    );
    assert!(!token.is_empty());

    // Validate and consume the token
    let user_id = ok(
        service
            .validate_token(&token, EmailTokenType::EmailBind)
            .await,
        "email bind token should validate",
    );
    assert_eq!(user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_expired_token_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = synctv_core::repository::EmailTokenRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = ok(
        user_repo.create(&make_user("svc_user_2")).await,
        "expired token test user should be created",
    );

    let token_str = synctv_common::snanoid!(64);
    let expired_at = Utc::now() - chrono::Duration::hours(1);
    ok(
        token_repo
            .create(&token_str, &user.id, EmailTokenType::EmailBind, expired_at)
            .await,
        "expired token fixture should be created",
    );

    // Service validation should fail
    let result = service
        .validate_token(&token_str, EmailTokenType::EmailBind)
        .await;
    assert!(result.is_err(), "Expired token should fail validation");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_wrong_type_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = ok(
        user_repo.create(&make_user("svc_user_3")).await,
        "wrong-type token test user should be created",
    );

    // Generate an email bind token
    let token = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailBind)
            .await,
        "email bind token should be generated",
    );

    // Validate with wrong type should fail
    let result = service
        .validate_token(&token, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_err(), "Wrong token type should fail validation");

    // Correct type should still work
    let user_id = ok(
        service
            .validate_token(&token, EmailTokenType::EmailBind)
            .await,
        "email bind token should validate after wrong-type attempt",
    );
    assert_eq!(user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_invalidate_user_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = ok(
        user_repo.create(&make_user("svc_user_4")).await,
        "token invalidation test user should be created",
    );

    // Generate multiple tokens
    let token1 = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailBind)
            .await,
        "first email bind token should be generated",
    );
    let token2 = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailBind)
            .await,
        "second email bind token should be generated",
    );

    // Invalidate all email bind tokens for this user
    ok(
        service
            .invalidate_user_tokens(&user.id, EmailTokenType::EmailBind)
            .await,
        "email bind tokens should be invalidated",
    );

    // Both tokens should now fail
    assert!(
        service
            .validate_token(&token1, EmailTokenType::EmailBind)
            .await
            .is_err(),
        "Token1 should be invalid after invalidate_user_tokens"
    );
    assert!(
        service
            .validate_token(&token2, EmailTokenType::EmailBind)
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

    let user = ok(
        user_repo.create(&make_user("svc_user_5")).await,
        "single-use token test user should be created",
    );

    let token = ok(
        service
            .generate_token(&user.id, EmailTokenType::PasswordReset)
            .await,
        "password reset token should be generated",
    );

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
        .map(|r| ok(r, "email token validation task should join"))
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
async fn test_service_concurrent_generation_replaces_unused_token_atomically() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());
    let service = EmailTokenService::new(pool.clone());

    let user = ok(
        user_repo
            .create(&make_user("svc_user_concurrent_generate"))
            .await,
        "concurrent generation test user should be created",
    );

    let mut handles = Vec::new();
    for _ in 0..8 {
        let svc = service.clone();
        let user_id = user.id;
        handles.push(tokio::spawn(async move {
            svc.generate_token(&user_id, EmailTokenType::PasswordReset)
                .await
        }));
    }

    let results = join_all(handles)
        .await
        .into_iter()
        .map(|result| ok(result, "email token generation task should join"))
        .collect::<Vec<_>>();

    assert!(
        results.iter().all(std::result::Result::is_ok),
        "concurrent generation should not fail with a uniqueness race: {results:?}"
    );

    let issued_tokens = results
        .into_iter()
        .map(|result| ok(result, "concurrent email token generation should succeed"))
        .collect::<Vec<_>>();
    let mut persisted = 0;
    for token in &issued_tokens {
        if ok(
            token_repo.get(token).await,
            "issued email token lookup should succeed",
        )
        .is_some()
        {
            persisted += 1;
        }
    }

    assert_eq!(
        persisted, 1,
        "only the latest generated token should remain persisted",
    );

    let remaining: i64 = ok(
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM auth_email_tokens WHERE user_id = $1 AND token_type = $2 AND used_at IS NULL"#,
            user.id.as_i64(),
            i16::from(EmailTokenType::PasswordReset)
        )
        .fetch_one(&pool)
        .await,
        "unused email token count query should succeed",
    );
    assert_eq!(remaining, 1, "there must be exactly one unused token row");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_validate_for_wrong_user_does_not_consume_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let owner = ok(
        user_repo.create(&make_user("svc_user_owner")).await,
        "email token owner should be created",
    );
    let other = ok(
        user_repo.create(&make_user("svc_user_other")).await,
        "email token other user should be created",
    );

    let token = ok(
        service
            .generate_token(&owner.id, EmailTokenType::PasswordReset)
            .await,
        "owner password reset token should be generated",
    );

    let wrong_user_attempt = service
        .validate_token_for_user(&token, EmailTokenType::PasswordReset, &other.id)
        .await;
    assert!(
        wrong_user_attempt.is_err(),
        "wrong-user validation must fail without consuming the token"
    );

    let owner_attempt = ok(
        service
            .validate_token_for_user(&token, EmailTokenType::PasswordReset, &owner.id)
            .await,
        "owner token validation should succeed",
    );
    assert_eq!(owner_attempt, owner.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_service_email_login_tokens_allow_multiple_active_codes_per_user() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let service = EmailTokenService::new(pool);

    let user = ok(
        user_repo
            .create(&make_user("svc_user_email_login_multi"))
            .await,
        "email login multi-token user should be created",
    );

    let first = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailLogin)
            .await,
        "first email login token should be generated",
    );
    let second = ok(
        service
            .generate_token(&user.id, EmailTokenType::EmailLogin)
            .await,
        "second email login token should be generated",
    );

    assert_ne!(
        first, second,
        "distinct login requests must issue distinct codes"
    );

    let first_user_id = ok(
        service
            .validate_token(&first, EmailTokenType::EmailLogin)
            .await,
        "first email login token should validate",
    );
    assert_eq!(first_user_id, user.id);

    let second_user_id = ok(
        service
            .validate_token(&second, EmailTokenType::EmailLogin)
            .await,
        "second email login token should validate",
    );
    assert_eq!(second_user_id, user.id);
}
