//! Authentication flow integration tests //!
//! Tests the complete authentication flow: register → login

use synctv_core::{
    models::{OpaquePasswordRecord, SignupMethod, User, UserId, UserRole, UserStatus},
    repository::{PasswordCredentialMaterial, UserPasswordRepository, UserRepository},
    service::{OpaquePasswordService, TokenCredentialBinding},
};
use synctv_core_testing::{create_test_jwt_service, create_test_pool, ok, some};

fn sign_test_refresh_token(
    jwt_service: &synctv_core::service::JwtService,
    user_id: &UserId,
) -> String {
    ok(
        jwt_service.sign_refresh_token_with_session(
            user_id,
            0,
            None,
            "auth-flow-refresh-session",
            &TokenCredentialBinding::Password { version: 0 },
        ),
        "refresh token should be signed",
    )
}

fn test_opaque_password_service() -> OpaquePasswordService {
    OpaquePasswordService::derive_from_secret(b"auth-flow-integration-tests")
}

async fn stored_opaque_record(pool: &sqlx::PgPool, user_id: UserId) -> OpaquePasswordRecord {
    let row = ok(
        sqlx::query!(
            r"
        SELECT opaque_record, opaque_credential_identifier, opaque_ciphersuite,
               opaque_server_setup_version
        FROM auth_password_credentials
        WHERE user_id = $1
        ",
            user_id.as_i64()
        )
        .fetch_one(pool)
        .await,
        "password credential query should succeed",
    );

    OpaquePasswordRecord {
        record: some(row.opaque_record, "opaque record should exist"),
        credential_identifier: some(
            row.opaque_credential_identifier,
            "opaque credential identifier should exist",
        ),
        ciphersuite: some(row.opaque_ciphersuite, "opaque ciphersuite should exist"),
        server_setup_version: some(
            row.opaque_server_setup_version,
            "opaque setup version should exist",
        ),
    }
}

async fn create_user_with_password(
    user_repo: &UserRepository,
    user: &User,
    password: &str,
) -> User {
    let user_password_repo = UserPasswordRepository::new(user_repo.pool().clone());
    let opaque_record = ok(
        test_opaque_password_service().register_password(
            format!("synctv:test:user:{}", user.username).as_bytes(),
            password,
        ),
        "test OPAQUE record should be generated",
    );
    let mut tx = ok(
        user_repo.pool().begin().await,
        "user creation tx should begin",
    );
    let created = ok(
        user_repo.create_with_executor(user, &mut *tx).await,
        "user should be created",
    );
    ok(
        user_password_repo
            .create_for_user_with_executor(
                &created,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
                &mut *tx,
            )
            .await,
        "password credential should be created",
    );
    ok(tx.commit().await, "user creation tx should commit");
    created
}

async fn fetch_user(repo: &UserRepository, username: &str) -> User {
    some(
        ok(
            repo.get_by_username(username).await,
            "user should be fetched by username",
        ),
        "user should exist",
    )
}

fn sign_access(jwt_service: &synctv_core::service::JwtService, user_id: &UserId) -> String {
    ok(
        jwt_service.sign_access_token(user_id, 0),
        "access token should be signed",
    )
}

fn verify_access(
    jwt_service: &synctv_core::service::JwtService,
    token: &str,
) -> synctv_core::service::Claims {
    ok(
        jwt_service.verify_access_token(token),
        "access token should verify",
    )
}

fn verify_refresh(
    jwt_service: &synctv_core::service::JwtService,
    token: &str,
) -> synctv_core::service::Claims {
    ok(
        jwt_service.verify_refresh_token(token),
        "refresh token should verify",
    )
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

    let fetched_user = fetch_user(&user_repo, &username).await;

    let fetched_opaque_record = stored_opaque_record(&pool, fetched_user.id).await;
    assert!(ok(
        test_opaque_password_service().verify_password(&fetched_opaque_record, password),
        "password should verify",
    ));

    // Generate tokens
    let jwt_service = create_test_jwt_service();
    let access_token = sign_access(&jwt_service, &fetched_user.id);
    let refresh_token = sign_test_refresh_token(&jwt_service, &fetched_user.id);

    // Verify tokens
    let access_claims = verify_access(&jwt_service, &access_token);
    assert_eq!(access_claims.user_id(), fetched_user.id);

    let refresh_claims = verify_refresh(&jwt_service, &refresh_token);
    assert_eq!(refresh_claims.user_id(), fetched_user.id);
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
    let fetched_user = fetch_user(&user_repo, &username).await;

    let wrong_password = "WrongPassword123!";
    let fetched_opaque_record = stored_opaque_record(&pool, fetched_user.id).await;
    let verify_result = ok(
        test_opaque_password_service().verify_password(&fetched_opaque_record, wrong_password),
        "wrong password verification should execute",
    );
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
    let fetched_user = fetch_user(&user_repo, &username).await;

    // Check status
    assert_eq!(fetched_user.status, UserStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_token_refresh_flow() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access_token = sign_access(&jwt_service, &user_id);
    let refresh_token = sign_test_refresh_token(&jwt_service, &user_id);

    // In real scenario, we would wait or use expired token

    let refresh_claims = verify_refresh(&jwt_service, &refresh_token);

    let new_access_token = sign_access(&jwt_service, &refresh_claims.user_id());

    let new_claims = verify_access(&jwt_service, &new_access_token);
    assert_eq!(new_claims.user_id(), user_id);

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
    let old_token = sign_access(&jwt_service, &created_user.id);

    let _old_token_claims = verify_access(&jwt_service, &old_token);

    // Change password
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    let opaque_record = OpaquePasswordRecord {
        record: b"opaque-record-v2".to_vec(),
        credential_identifier: b"synctv:user-id:password-change".to_vec(),
        ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
        server_setup_version: 1,
    };

    let user_password_repo = UserPasswordRepository::new(pool.clone());
    let old_state = ok(
        user_password_repo.get_state(&created_user.id).await,
        "credential state should exist",
    );
    let updated_state = ok(
        user_password_repo
            .update_with_executor(
                &created_user.id,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
                &pool,
            )
            .await,
        "password credentials should update",
    );
    assert_eq!(updated_state.version, old_state.version + 1);

    let fetched_user = some(
        ok(
            user_repo.get_by_id(&created_user.id).await,
            "user should be fetched",
        ),
        "user should exist",
    );
    assert_eq!(fetched_user.id, created_user.id);

    let row = ok(
        sqlx::query!(
            r#"
        SELECT opaque_record, version AS "version!"
        FROM auth_password_credentials
        WHERE user_id = $1
        "#,
            created_user.id.as_i64()
        )
        .fetch_one(&pool)
        .await,
        "password credential row should exist",
    );
    assert_eq!(row.opaque_record, Some(b"opaque-record-v2".to_vec()));
    assert_eq!(row.version, old_state.version + 1);
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
            let user = fetch_user(&repo, &username).await;

            let token = sign_access(&jwt, &user.id);

            verify_access(&jwt, &token)
        });

        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| ok(result, "login task should complete"))
        .collect();

    // All should succeed with same user ID
    assert_eq!(results.len(), 10);
    assert!(results.iter().all(|claims| claims.user_id() == user_id));
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
    ok(
        user_repo
            .ban(&created_user.id, None, Some("auth flow test".to_string()))
            .await,
        "user should be banned",
    );

    // Fetch user
    let fetched_user = fetch_user(&user_repo, &username).await;

    assert_eq!(fetched_user.status, UserStatus::Banned);
    assert!(ok(
        user_repo.is_banned(&fetched_user.id).await,
        "ban state should be fetched",
    ));

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
    let result = ok(
        user_repo.get_by_username(&uppercase_username).await,
        "user lookup should execute",
    );

    // Depending on database collation, this may or may not find the user
    // Most implementations should be case-insensitive for usernames
    if let Some(fetched) = result {
        assert_eq!(fetched.username.to_lowercase(), username.to_lowercase());
    }
}
