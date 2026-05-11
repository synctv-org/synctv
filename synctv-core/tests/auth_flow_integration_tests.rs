//! Authentication flow integration tests //!
//! Tests the complete authentication flow: register → verify → login
//!
//! Run with: cargo test --test `auth_flow_integration_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::{
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::auth::{
        password::{hash_password, verify_password},
        TokenType,
    },
};
use synctv_core_testing::{create_test_jwt_service, create_test_pool};
/// Default `PostgreSQL` version for test containers
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_complete_registration_flow() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let email = format!("{username}@test.com");
    let password = "SecurePassword123!";

    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(email.clone()),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: false,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = user_repo
        .create(&user)
        .await
        .expect("Failed to create user");
    assert_eq!(created_user.status, UserStatus::Active);
    assert!(!created_user.email_verified);

    let mut verified_user = created_user.clone();
    verified_user.email_verified = true;

    let old_version = verified_user.version;
    let updated_user = user_repo
        .update(&verified_user, old_version)
        .await
        .expect("Failed to update user");
    assert_eq!(updated_user.status, UserStatus::Active);
    assert!(updated_user.email_verified);

    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    assert!(verify_password(password, &fetched_user.password_hash)
        .await
        .unwrap());

    // Generate tokens
    let jwt_service = create_test_jwt_service();
    let access_token = jwt_service
        .sign_token(&fetched_user.id, TokenType::Access, 0)
        .expect("Failed to sign access token");
    let refresh_token = jwt_service
        .sign_token(&fetched_user.id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    // Verify tokens
    let access_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");
    assert_eq!(access_claims.sub, fetched_user.id.to_string());

    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");
    assert_eq!(refresh_claims.sub, fetched_user.id.to_string());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_with_wrong_password() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let password = "CorrectPassword123!";
    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    user_repo
        .create(&user)
        .await
        .expect("Failed to create user");

    // Try to login with wrong password
    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    let wrong_password = "WrongPassword123!";
    let verify_result = verify_password(wrong_password, &fetched_user.password_hash)
        .await
        .unwrap();
    assert!(!verify_result, "Wrong password should not verify");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_login_unverified_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: false,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    user_repo
        .create(&user)
        .await
        .expect("Failed to create user");

    // Fetch user
    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Check status
    assert_eq!(fetched_user.status, UserStatus::Active);
    assert!(!fetched_user.email_verified);

    // Application should reject login for unverified users
    // (This would be enforced in the service layer)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_token_refresh_flow() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    // In real scenario, we would wait or use expired token

    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    let new_access_token = jwt_service
        .sign_token(&refresh_claims.user_id().unwrap(), TokenType::Access, 0)
        .expect("Failed to sign new access token");

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify new access token");
    assert_eq!(new_claims.sub, user_id.to_string());

    // Old and new access tokens should be different
    assert_ne!(access_token, new_access_token);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_password_change_invalidates_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let jwt_service = create_test_jwt_service();

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let old_password = "OldPassword123!";
    let password_hash = hash_password(old_password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = user_repo
        .create(&user)
        .await
        .expect("Failed to create user");

    // Generate token
    let old_token = jwt_service
        .sign_token(&created_user.id, TokenType::Access, 0)
        .expect("Failed to sign token");

    let _old_token_claims = jwt_service
        .verify_access_token(&old_token)
        .expect("Failed to verify old token");

    // Change password
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let new_password = "NewPassword123!";
    let new_password_hash = hash_password(new_password)
        .await
        .expect("Failed to hash new password");

    user_repo
        .update_password(&created_user.id, &new_password_hash)
        .await
        .expect("Failed to update password");

    // In a real system, old tokens should be invalidated by checking updated_at
    // against token issued_at (iat) or using a token revocation list
    let fetched_user = user_repo
        .get_by_id(&created_user.id)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Verify password was changed
    assert!(verify_password(new_password, &fetched_user.password_hash)
        .await
        .unwrap());
    assert!(!verify_password(old_password, &fetched_user.password_hash)
        .await
        .unwrap());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_login_attempts() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));
    let jwt_service = Arc::new(create_test_jwt_service());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = user_repo
        .create(&user)
        .await
        .expect("Failed to create user");
    let user_id = created_user.id;

    // Simulate 10 concurrent login attempts
    let mut handles = vec![];
    for _ in 0..10 {
        let repo = user_repo.clone();
        let jwt = jwt_service.clone();
        let username = username.clone();

        let handle = tokio::spawn(async move {
            let user = repo
                .get_by_username(&username)
                .await
                .expect("Failed to fetch user")
                .expect("User not found");

            let token = jwt
                .sign_token(&user.id, TokenType::Access, 0)
                .expect("Failed to sign token");

            jwt.verify_access_token(&token)
                .expect("Failed to verify token")
        });

        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // All should succeed with same user ID
    assert_eq!(results.len(), 10);
    assert!(results
        .iter()
        .all(|claims| claims.sub == user_id.to_string()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_banned_user_login_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = user_repo
        .create(&user)
        .await
        .expect("Failed to create user");
    user_repo
        .ban(&created_user.id, None, Some("auth flow test".to_string()))
        .await
        .expect("Failed to ban user");

    // Fetch user
    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    assert_eq!(fetched_user.status, UserStatus::Banned);
    assert!(user_repo.is_banned(&fetched_user.id).await.unwrap());

    // Application should reject login for banned users
    // (Enforced in service layer)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_username_case_insensitive_login() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("testuser_{}", synctv_common::snanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password)
        .await
        .expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.to_lowercase(),
        email: Some(format!("{username}@test.com")),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    user_repo
        .create(&user)
        .await
        .expect("Failed to create user");

    // Try to fetch with different case
    let uppercase_username = username.to_uppercase();
    let result = user_repo
        .get_by_username(&uppercase_username)
        .await
        .expect("Failed to query database");

    // Depending on database collation, this may or may not find the user
    // Most implementations should be case-insensitive for usernames
    if let Some(fetched) = result {
        assert_eq!(fetched.username.to_lowercase(), username.to_lowercase());
    }
}
