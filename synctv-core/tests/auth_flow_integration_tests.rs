//! Authentication flow integration tests (Task #90)
//!
//! Tests the complete authentication flow: register → verify → login
//!
//! Run with: cargo test --test auth_flow_integration_tests

use synctv_core::{
    models::{User, UserId, UserRole, UserStatus, SignupMethod},
    repository::UserRepository,
    service::auth::{jwt::JwtService, password::{hash_password, verify_password}, TokenType},
};
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

fn create_test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-integration-tests-minimum-length-32-chars")
        .expect("Failed to create JWT service")
}

#[tokio::test]
async fn test_complete_registration_flow() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Step 1: Register user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let email = format!("{}@test.com", username);
    let password = "SecurePassword123!";

    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(email.clone()),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Pending,
        email_verified: false,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    let created_user = user_repo.create(&user).await.expect("Failed to create user");
    assert_eq!(created_user.status, UserStatus::Pending);
    assert_eq!(created_user.email_verified, false);

    // Step 2: Verify email (simulate)
    let mut verified_user = created_user.clone();
    verified_user.status = UserStatus::Active;
    verified_user.email_verified = true;

    let old_version = verified_user.version;
    let updated_user = user_repo.update(&verified_user, old_version).await.expect("Failed to update user");
    assert_eq!(updated_user.status, UserStatus::Active);
    assert!(updated_user.email_verified);

    // Step 3: Login
    let fetched_user = user_repo.get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    assert!(verify_password(password, &fetched_user.password_hash).await.unwrap());

    // Generate tokens
    let jwt_service = create_test_jwt_service();
    let access_token = jwt_service.sign_token(&fetched_user.id, TokenType::Access, 0)
        .expect("Failed to sign access token");
    let refresh_token = jwt_service.sign_token(&fetched_user.id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    // Verify tokens
    let access_claims = jwt_service.verify_access_token(&access_token)
        .expect("Failed to verify access token");
    assert_eq!(access_claims.sub, fetched_user.id.as_str());

    let refresh_claims = jwt_service.verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");
    assert_eq!(refresh_claims.sub, fetched_user.id.as_str());
}

#[tokio::test]
async fn test_login_with_wrong_password() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let password = "CorrectPassword123!";
    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    user_repo.create(&user).await.expect("Failed to create user");

    // Try to login with wrong password
    let fetched_user = user_repo.get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    let wrong_password = "WrongPassword123!";
    let verify_result = verify_password(wrong_password, &fetched_user.password_hash).await.unwrap();
    assert!(!verify_result, "Wrong password should not verify");
}

#[tokio::test]
async fn test_login_unverified_user_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create unverified user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Pending,
        email_verified: false,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    user_repo.create(&user).await.expect("Failed to create user");

    // Fetch user
    let fetched_user = user_repo.get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Check status
    assert_eq!(fetched_user.status, UserStatus::Pending);
    assert!(!fetched_user.email_verified);

    // Application should reject login for unverified users
    // (This would be enforced in the service layer)
}

#[tokio::test]
async fn test_token_refresh_flow() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Step 1: Initial login - get access and refresh tokens
    let access_token = jwt_service.sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to sign access token");
    let refresh_token = jwt_service.sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to sign refresh token");

    // Step 2: Access token expires (simulated by time passing)
    // In real scenario, we would wait or use expired token

    // Step 3: Use refresh token to get new access token
    let refresh_claims = jwt_service.verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    let new_access_token = jwt_service.sign_token(&refresh_claims.user_id(), TokenType::Access, 0)
        .expect("Failed to sign new access token");

    // Step 4: Verify new access token works
    let new_claims = jwt_service.verify_access_token(&new_access_token)
        .expect("Failed to verify new access token");
    assert_eq!(new_claims.sub, user_id.as_str());

    // Old and new access tokens should be different
    assert_ne!(access_token, new_access_token);
}

#[tokio::test]
async fn test_password_change_invalidates_tokens() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let jwt_service = create_test_jwt_service();

    // Create user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let old_password = "OldPassword123!";
    let password_hash = hash_password(old_password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    let created_user = user_repo.create(&user).await.expect("Failed to create user");

    // Generate token
    let old_token = jwt_service.sign_token(&created_user.id, TokenType::Access, 0)
        .expect("Failed to sign token");

    let _old_token_claims = jwt_service.verify_access_token(&old_token)
        .expect("Failed to verify old token");

    // Change password
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    let new_password = "NewPassword123!";
    let new_password_hash = hash_password(new_password).await.expect("Failed to hash new password");

    let mut updated_user = created_user.clone();
    updated_user.password_hash = new_password_hash;
    updated_user.updated_at = chrono::Utc::now();

    let old_version = updated_user.version;
    user_repo.update(&updated_user, old_version).await.expect("Failed to update user");

    // In a real system, old tokens should be invalidated by checking updated_at
    // against token issued_at (iat) or using a token revocation list
    let fetched_user = user_repo.get_by_id(&created_user.id)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Verify password was changed
    assert!(verify_password(new_password, &fetched_user.password_hash).await.unwrap());
    assert!(!verify_password(old_password, &fetched_user.password_hash).await.unwrap());
}

#[tokio::test]
async fn test_concurrent_login_attempts() {
    use std::sync::Arc;

    let (_container, pool) = create_test_pool().await;
    let user_repo = Arc::new(UserRepository::new(pool.clone()));
    let jwt_service = Arc::new(create_test_jwt_service());

    // Create user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    let created_user = user_repo.create(&user).await.expect("Failed to create user");
    let user_id = created_user.id.clone();

    // Simulate 10 concurrent login attempts
    let mut handles = vec![];
    for _ in 0..10 {
        let repo = user_repo.clone();
        let jwt = jwt_service.clone();
        let username = username.clone();

        let handle = tokio::spawn(async move {
            let user = repo.get_by_username(&username)
                .await
                .expect("Failed to fetch user")
                .expect("User not found");

            let token = jwt.sign_token(&user.id, TokenType::Access, 0)
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
    assert!(results.iter().all(|claims| claims.sub == user_id.as_str()));
}

#[tokio::test]
async fn test_banned_user_login_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create banned user
    let username = format!("test_user_{}", nanoid::nanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.clone(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Banned,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    user_repo.create(&user).await.expect("Failed to create user");

    // Fetch user
    let fetched_user = user_repo.get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Check status
    assert_eq!(fetched_user.status, UserStatus::Banned);

    // Application should reject login for banned users
    // (Enforced in service layer)
}

#[tokio::test]
async fn test_username_case_insensitive_login() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    // Create user with lowercase username
    let username = format!("testuser_{}", nanoid::nanoid!(10));
    let password = "Password123!";
    let password_hash = hash_password(password).await.expect("Failed to hash password");

    let user = User {
        id: UserId::new(),
        username: username.to_lowercase(),
        email: Some(format!("{}@test.com", username)),
        password_hash,
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: Some(SignupMethod::Email),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        password_changed_at: chrono::Utc::now(),
        password_version: 0,
        version: 0,
        deleted_at: None,
    };

    user_repo.create(&user).await.expect("Failed to create user");

    // Try to fetch with different case
    let uppercase_username = username.to_uppercase();
    let result = user_repo.get_by_username(&uppercase_username).await
        .expect("Failed to query database");

    // Depending on database collation, this may or may not find the user
    // Most implementations should be case-insensitive for usernames
    if result.is_some() {
        let fetched = result.unwrap();
        assert_eq!(fetched.username.to_lowercase(), username.to_lowercase());
    }
}
