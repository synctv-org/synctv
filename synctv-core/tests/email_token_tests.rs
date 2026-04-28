//! Email token lifecycle integration tests
//!
//! Tests token creation, usage (mark as used), expiry checking, and cleanup.
//! Requires real `PostgreSQL` via testcontainers.
//!
//! Run with: cargo test --test `email_token_tests`
#![allow(clippy::unwrap_used)]

use chrono::{Duration, Utc};
use synctv_core::{
    models::{User, UserId, UserRole, UserStatus},
    repository::{EmailTokenRepository, UserRepository},
    service::{
        email_token::{EmailTokenService, EmailTokenType},
        EmailConfig, EmailService,
    },
};
use synctv_core_testing::create_test_pool;
/// Default `PostgreSQL` version for test containers
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
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_and_get_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_1")).await.unwrap();

    let token_str = synctv_common::snanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    let created = token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    assert_ne!(
        created.token, token_str,
        "email tokens must never be stored in plaintext"
    );
    assert_eq!(created.user_id, user.id);
    assert_eq!(
        created.token_type,
        i16::from(EmailTokenType::EmailVerification)
    );
    assert!(created.used_at.is_none());

    // Get the token
    let fetched = token_repo.get(&token_str).await.unwrap();
    assert!(fetched.is_some());
    let fetched = fetched.unwrap();
    assert_ne!(
        fetched.token, token_str,
        "repository lookup should hash the raw token before querying"
    );
    assert_eq!(fetched.user_id, user.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_token_as_used() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_2")).await.unwrap();

    let token_str = synctv_common::snanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    // Mark as used
    let used = token_repo.mark_as_used(&token_str).await.unwrap();
    assert!(used.used_at.is_some(), "Token should have used_at set");

    // Verify it's marked as used when fetched
    let fetched = token_repo.get(&token_str).await.unwrap().unwrap();
    assert!(fetched.used_at.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_validate_and_consume_valid_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_3")).await.unwrap();

    let token_str = synctv_common::snanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::PasswordReset,
            expires_at,
        )
        .await
        .unwrap();

    // Validate and consume
    let consumed = token_repo
        .validate_and_consume(&token_str, EmailTokenType::PasswordReset)
        .await
        .unwrap();
    assert!(consumed.is_some(), "Valid token should be consumed");
    assert!(consumed.unwrap().used_at.is_some());

    // Consuming again should return None (already used)
    let consumed_again = token_repo
        .validate_and_consume(&token_str, EmailTokenType::PasswordReset)
        .await
        .unwrap();
    assert!(
        consumed_again.is_none(),
        "Already-used token should not be consumable"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_validate_wrong_type_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_4")).await.unwrap();

    let token_str = synctv_common::snanoid!(32);
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    // Try to consume as PasswordReset - wrong type
    let result = token_repo
        .validate_and_consume(&token_str, EmailTokenType::PasswordReset)
        .await
        .unwrap();
    assert!(
        result.is_none(),
        "Token with wrong type should not be consumed"
    );

    // The original type should still work
    let result = token_repo
        .validate_and_consume(&token_str, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert!(result.is_some());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_expired_token_not_consumable() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_5")).await.unwrap();

    let token_str = synctv_common::snanoid!(32);
    // Set expiry in the past
    let expires_at = Utc::now() - Duration::hours(1);

    token_repo
        .create(
            &token_str,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    // Should not be consumable (expired)
    let result = token_repo
        .validate_and_consume(&token_str, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert!(result.is_none(), "Expired token should not be consumable");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_cleanup_expired_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    // "one unused token per user + type" invariant enforced by the DB.
    for i in 0..3 {
        let user = user_repo
            .create(&make_user(&format!("token_user_6_{i}")))
            .await
            .unwrap();
        let token_str = synctv_common::snanoid!(32);
        let expires_at = Utc::now() - Duration::hours(i + 1);
        token_repo
            .create(
                &token_str,
                &user.id,
                EmailTokenType::EmailVerification,
                expires_at,
            )
            .await
            .unwrap();
    }

    let valid_user = user_repo
        .create(&make_user("token_user_6_valid"))
        .await
        .unwrap();

    let valid_token = synctv_common::snanoid!(32);
    let valid_expires = Utc::now() + Duration::hours(24);
    token_repo
        .create(
            &valid_token,
            &valid_user.id,
            EmailTokenType::EmailVerification,
            valid_expires,
        )
        .await
        .unwrap();

    // Cleanup expired
    let cleaned = token_repo.cleanup_expired().await.unwrap();
    assert_eq!(cleaned, 3, "Should clean up 3 expired tokens");

    // Valid token should still exist
    let fetched = token_repo.get(&valid_token).await.unwrap();
    assert!(fetched.is_some(), "Valid token should survive cleanup");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_user_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo.create(&make_user("token_user_7")).await.unwrap();
    let expires_at = Utc::now() + Duration::hours(24);

    let used_verification_token = synctv_common::snanoid!(32);
    token_repo
        .create(
            &used_verification_token,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();
    token_repo
        .mark_as_used(&used_verification_token)
        .await
        .unwrap();

    let active_verification_token = synctv_common::snanoid!(32);
    token_repo
        .create(
            &active_verification_token,
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    let password_reset_token = synctv_common::snanoid!(32);
    token_repo
        .create(
            &password_reset_token,
            &user.id,
            EmailTokenType::PasswordReset,
            expires_at,
        )
        .await
        .unwrap();

    // Delete current unused verification tokens for the user.
    let deleted = token_repo
        .delete_user_tokens(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    assert_eq!(deleted, 1);

    assert!(
        token_repo
            .get(&active_verification_token)
            .await
            .unwrap()
            .is_none(),
        "delete_user_tokens should remove the active unused token",
    );
    assert!(
        token_repo
            .get(&used_verification_token)
            .await
            .unwrap()
            .is_some(),
        "delete_user_tokens should not remove already-used historical tokens",
    );
    assert!(
        token_repo
            .get(&password_reset_token)
            .await
            .unwrap()
            .is_some(),
        "delete_user_tokens should not remove other token types",
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_nonexistent_token() {
    let (_container, pool) = create_test_pool().await;
    let token_repo = EmailTokenRepository::new(pool.clone());

    let result = token_repo.get("nonexistent_token_value").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_allows_multiple_unused_tokens_for_same_user_and_type() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_unique_invariant"))
        .await
        .unwrap();
    let expires_at = Utc::now() + Duration::hours(24);

    token_repo
        .create(
            "first-raw-token",
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    let second = token_repo
        .create(
            "second-raw-token",
            &user.id,
            EmailTokenType::EmailVerification,
            expires_at,
        )
        .await
        .unwrap();

    assert_eq!(second.user_id, user.id);
    assert_eq!(
        second.token_type,
        i16::from(EmailTokenType::EmailVerification)
    );

    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM auth_email_tokens WHERE user_id = $1 AND token_type = $2 AND used_at IS NULL",
    )
    .bind(user.id)
    .bind(i16::from(EmailTokenType::EmailVerification))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        remaining, 2,
        "repository create should allow multiple unused tokens for the same user and type",
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_failed_verification_email_send_does_not_leave_valid_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool);

    let user = user_repo
        .create(&make_user("token_send_failure"))
        .await
        .unwrap();
    let email = user.email.clone().unwrap();
    let email_service = EmailService::new(Some(EmailConfig {
        smtp_host: "127.0.0.1".to_string(),
        smtp_port: 9,
        smtp_username: "test".to_string(),
        smtp_password: "test".to_string(),
        from_email: "noreply@example.com".to_string(),
        from_name: "SyncTV".to_string(),
        use_tls: false,
    }))
    .unwrap();

    let result = email_service
        .send_verification_email(&email, &token_service, &user.id)
        .await;
    assert!(result.is_err(), "SMTP failure should surface as an error");

    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM auth_email_tokens WHERE user_id = $1 AND token_type = $2 AND used_at IS NULL",
    )
    .bind(user.id)
    .bind(i16::from(EmailTokenType::EmailVerification))
    .fetch_one(user_repo.pool())
    .await
    .unwrap();
    assert_eq!(
        remaining, 0,
        "failed email delivery must not leave an unused verification token behind"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_invalidating_old_token_does_not_remove_newer_replacement() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool);

    let user = user_repo
        .create(&make_user("token_specific_invalidation"))
        .await
        .unwrap();

    let first_token = token_service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();
    let second_token = token_service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    token_service
        .invalidate_specific_token(&first_token, &user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    assert!(
        token_service
            .validate_token(&second_token, EmailTokenType::EmailVerification)
            .await
            .is_ok(),
        "cleaning up a superseded token must not remove the current replacement",
    );
    assert!(
        token_service
            .validate_token(&first_token, EmailTokenType::EmailVerification)
            .await
            .is_err(),
        "the superseded token should stay invalid",
    );
}

/// Test that generating a new token invalidates previous tokens of the same type
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_generate_token_invalidates_previous_tokens_same_type() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_invalidation_1"))
        .await
        .unwrap();

    // Generate first token
    let first_token = token_service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // First token should be valid
    let result = token_service
        .validate_token(&first_token, EmailTokenType::PasswordReset)
        .await;
    assert!(
        result.is_ok(),
        "First token should be valid before generating second"
    );

    // Generate second token (should invalidate first)
    let second_token = token_service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Second token should be valid
    let result = token_service
        .validate_token(&second_token, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_ok(), "Second token should be valid");

    // First token should now be invalid
    let result = token_service
        .validate_token(&first_token, EmailTokenType::PasswordReset)
        .await;
    assert!(
        result.is_err(),
        "First token should be invalidated after generating second"
    );
}

/// Test that tokens of different types are independent
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_different_token_types_are_independent() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_invalidation_2"))
        .await
        .unwrap();

    // Generate email verification token
    let email_token = token_service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Generate password reset token
    let reset_token = token_service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Both should be valid (different types)
    let result = token_service
        .validate_token(&email_token, EmailTokenType::EmailVerification)
        .await;
    assert!(result.is_ok(), "Email token should still be valid");

    let result = token_service
        .validate_token(&reset_token, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_ok(), "Password reset token should be valid");
}

/// Test that generating new password reset token doesn't affect email verification token
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_reset_regeneration_preserves_email_token() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_invalidation_3"))
        .await
        .unwrap();

    // Generate email verification token
    let email_token = token_service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Generate first password reset token
    let reset1 = token_service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Generate second password reset token (should invalidate first reset, but not email)
    let reset2 = token_service
        .generate_token(&user.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Second reset token should be valid
    let result = token_service
        .validate_token(&reset2, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_ok(), "Second reset token should be valid");

    // First reset token should be invalid
    let result = token_service
        .validate_token(&reset1, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_err(), "First reset token should be invalidated");

    // Email token should still be valid
    let result = token_service
        .validate_token(&email_token, EmailTokenType::EmailVerification)
        .await;
    assert!(
        result.is_ok(),
        "Email token should still be valid after reset token regeneration"
    );
}

/// Test multiple token generations, only last is valid
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_multiple_token_generations_only_last_valid() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_invalidation_4"))
        .await
        .unwrap();

    let mut tokens = Vec::new();

    // Generate 5 tokens
    for _ in 0..5 {
        let token = token_service
            .generate_token(&user.id, EmailTokenType::PasswordReset)
            .await
            .unwrap();
        tokens.push(token);
    }

    // Only the last token should be valid
    let last_token = tokens.last().unwrap();
    let result = token_service
        .validate_token(last_token, EmailTokenType::PasswordReset)
        .await;
    assert!(result.is_ok(), "Last generated token should be valid");

    // All previous tokens should be invalid
    for (i, token) in tokens.iter().enumerate() {
        if i < tokens.len() - 1 {
            let result = token_service
                .validate_token(token, EmailTokenType::PasswordReset)
                .await;
            assert!(result.is_err(), "Token {i} should be invalid");
        }
    }
}

/// Test that two users can have valid tokens simultaneously
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_two_users_independent_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_repo = EmailTokenRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user1 = user_repo.create(&make_user("token_user_a")).await.unwrap();
    let user2 = user_repo.create(&make_user("token_user_b")).await.unwrap();

    // Generate tokens for both users
    let token1 = token_service
        .generate_token(&user1.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    let token2 = token_service
        .generate_token(&user2.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // Both should exist and be unused (check via repo, not consuming validate)
    let fetched1 = token_repo.get(&token1).await.unwrap();
    assert!(fetched1.is_some(), "User 1 token should exist");
    assert!(
        fetched1.unwrap().used_at.is_none(),
        "User 1 token should be unused"
    );

    let fetched2 = token_repo.get(&token2).await.unwrap();
    assert!(fetched2.is_some(), "User 2 token should exist");
    assert!(
        fetched2.unwrap().used_at.is_none(),
        "User 2 token should be unused"
    );

    // Regenerating for user1 should not affect user2
    let token1_new = token_service
        .generate_token(&user1.id, EmailTokenType::PasswordReset)
        .await
        .unwrap();

    // user1's old token should be deleted
    let fetched1_old = token_repo.get(&token1).await.unwrap();
    assert!(fetched1_old.is_none(), "User 1 old token should be deleted");

    // user1's new token should exist
    let fetched1_new = token_repo.get(&token1_new).await.unwrap();
    assert!(fetched1_new.is_some(), "User 1 new token should exist");

    // user2's token should still exist and be unused
    let fetched2_again = token_repo.get(&token2).await.unwrap();
    assert!(fetched2_again.is_some(), "User 2 token should still exist");
    assert!(
        fetched2_again.unwrap().used_at.is_none(),
        "User 2 token should still be unused"
    );
}

/// Test that manual invalidation works
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_manual_token_invalidation() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let token_service = EmailTokenService::new(pool.clone());

    let user = user_repo
        .create(&make_user("token_manual_invalidation"))
        .await
        .unwrap();

    // Generate token
    let token = token_service
        .generate_token(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    let result = token_service
        .validate_token(&token, EmailTokenType::EmailVerification)
        .await;
    assert!(result.is_ok());

    // Manually invalidate
    token_service
        .invalidate_user_tokens(&user.id, EmailTokenType::EmailVerification)
        .await
        .unwrap();

    // Should now be invalid
    let result = token_service
        .validate_token(&token, EmailTokenType::EmailVerification)
        .await;
    assert!(
        result.is_err(),
        "Token should be invalid after manual invalidation"
    );
}
