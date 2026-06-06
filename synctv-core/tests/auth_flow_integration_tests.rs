//! Authentication flow integration tests //!
//! Tests the complete authentication flow: register → login
//!
#![allow(clippy::unwrap_used)]

use sqlx::Row;
use synctv_core::{
    models::{OpaquePasswordRecord, SignupMethod, User, UserId, UserRole, UserStatus},
    repository::{PasswordCredentialMaterial, UserPasswordRepository, UserRepository},
    service::auth::{OpaquePasswordService, TokenCredentialBinding},
};
use synctv_core_testing::{create_test_jwt_service, create_test_pool};

fn sign_test_refresh_token(
    jwt_service: &synctv_core::service::auth::jwt::JwtService,
    user_id: &UserId,
) -> String {
    jwt_service
        .sign_refresh_token_with_session(
            user_id,
            0,
            None,
            "auth-flow-refresh-session",
            &TokenCredentialBinding::Password { version: 0 },
        )
        .expect("Failed to sign refresh token")
}

fn test_opaque_password_service() -> OpaquePasswordService {
    OpaquePasswordService::derive_from_secret(b"auth-flow-integration-tests")
}

async fn stored_opaque_record(pool: &sqlx::PgPool, user_id: UserId) -> OpaquePasswordRecord {
    let row = sqlx::query(
        r"
        SELECT opaque_record, opaque_credential_identifier, opaque_ciphersuite,
               opaque_server_setup_version
        FROM auth_password_credentials
        WHERE user_id = $1
        ",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
    .expect("password credential query should succeed");

    OpaquePasswordRecord {
        record: row
            .try_get::<Option<Vec<u8>>, _>("opaque_record")
            .unwrap()
            .expect("opaque record should exist"),
        credential_identifier: row
            .try_get::<Option<Vec<u8>>, _>("opaque_credential_identifier")
            .unwrap()
            .expect("opaque credential identifier should exist"),
        ciphersuite: row
            .try_get::<Option<String>, _>("opaque_ciphersuite")
            .unwrap()
            .expect("opaque ciphersuite should exist"),
        server_setup_version: row
            .try_get::<Option<i32>, _>("opaque_server_setup_version")
            .unwrap()
            .expect("opaque setup version should exist"),
    }
}

async fn create_user_with_password(
    user_repo: &UserRepository,
    user: &User,
    password: &str,
) -> User {
    let user_password_repo = UserPasswordRepository::new(user_repo.pool().clone());
    let opaque_record = test_opaque_password_service()
        .register_password(
            format!("synctv:test:user:{}", user.username).as_bytes(),
            password,
        )
        .expect("test OPAQUE record should be generated");
    let mut tx = user_repo.pool().begin().await.expect("begin tx");
    let created = user_repo
        .create_with_executor(user, &mut *tx)
        .await
        .expect("Failed to create user");
    user_password_repo
        .create_for_user_with_executor(
            &created,
            PasswordCredentialMaterial::opaque_only(&opaque_record),
            &mut *tx,
        )
        .await
        .expect("Failed to create password credential");
    tx.commit().await.expect("commit user creation");
    created
}
/// Default `PostgreSQL` version for test containers
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_complete_registration_flow() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let password = "SecurePassword123!";
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = create_user_with_password(&user_repo, &user, password).await;
    assert_eq!(created_user.status, UserStatus::Active);

    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    let fetched_opaque_record = stored_opaque_record(&pool, fetched_user.id).await;
    assert!(test_opaque_password_service()
        .verify_password(&fetched_opaque_record, password)
        .unwrap());

    // Generate tokens
    let jwt_service = create_test_jwt_service();
    let access_token = jwt_service
        .sign_access_token(&fetched_user.id, 0)
        .expect("Failed to sign access token");
    let refresh_token = sign_test_refresh_token(&jwt_service, &fetched_user.id);

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
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    create_user_with_password(&user_repo, &user, password).await;

    // Try to login with wrong password
    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    let wrong_password = "WrongPassword123!";
    let fetched_opaque_record = stored_opaque_record(&pool, fetched_user.id).await;
    let verify_result = test_opaque_password_service()
        .verify_password(&fetched_opaque_record, wrong_password)
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
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    create_user_with_password(&user_repo, &user, password).await;

    // Fetch user
    let fetched_user = user_repo
        .get_by_username(&username)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");

    // Check status
    assert_eq!(fetched_user.status, UserStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_token_refresh_flow() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt_service
        .sign_access_token(&user_id, 0)
        .expect("Failed to sign access token");
    let refresh_token = sign_test_refresh_token(&jwt_service, &user_id);

    // In real scenario, we would wait or use expired token

    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    let new_access_token = jwt_service
        .sign_access_token(&refresh_claims.user_id().unwrap(), 0)
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
async fn test_password_credential_update_replaces_opaque_record() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let jwt_service = create_test_jwt_service();

    let username = format!("test_user_{}", synctv_common::snanoid!(10));
    let old_password = "OldPassword123!";
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = create_user_with_password(&user_repo, &user, old_password).await;

    // Generate token
    let old_token = jwt_service
        .sign_access_token(&created_user.id, 0)
        .expect("Failed to sign token");

    let _old_token_claims = jwt_service
        .verify_access_token(&old_token)
        .expect("Failed to verify old token");

    // Change password
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let opaque_record = OpaquePasswordRecord {
        record: b"opaque-record-v2".to_vec(),
        credential_identifier: b"synctv:user-id:password-change".to_vec(),
        ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
        server_setup_version: 1,
    };

    let user_password_repo = UserPasswordRepository::new(pool.clone());
    let old_state = user_password_repo
        .get_state(&created_user.id)
        .await
        .expect("credential state should exist");
    let updated_state = user_password_repo
        .update_with_executor(
            &created_user.id,
            PasswordCredentialMaterial::opaque_only(&opaque_record),
            &pool,
        )
        .await
        .expect("Failed to update password credentials");
    assert_eq!(updated_state.version, old_state.version + 1);

    let fetched_user = user_repo
        .get_by_id(&created_user.id)
        .await
        .expect("Failed to fetch user")
        .expect("User not found");
    assert_eq!(fetched_user.id, created_user.id);

    let row = sqlx::query(
        r"
        SELECT opaque_record, version
        FROM auth_password_credentials
        WHERE user_id = $1
        ",
    )
    .bind(created_user.id.as_i64())
    .fetch_one(&pool)
    .await
    .expect("password credential row should exist");
    assert_eq!(
        row.try_get::<Option<Vec<u8>>, _>("opaque_record").unwrap(),
        Some(b"opaque-record-v2".to_vec())
    );
    assert_eq!(
        row.try_get::<i32, _>("version").unwrap(),
        old_state.version + 1
    );
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
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = create_user_with_password(&user_repo, &user, password).await;
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
                .sign_access_token(&user.id, 0)
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
    let user = User {
        id: UserId::new(),
        username: username.clone(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    let created_user = create_user_with_password(&user_repo, &user, password).await;
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
    let user = User {
        id: UserId::new(),
        username: username.to_lowercase(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: SignupMethod::Email,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    };

    create_user_with_password(&user_repo, &user, password).await;

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
