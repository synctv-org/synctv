//! UserRepository integration tests
//!
//! Tests optimistic locking, soft-delete interactions, and batch queries.
//!
//! Run with: cargo test --test user_repository_tests

use synctv_core::{
    models::{
        User, UserId, UserRole, UserStatus,
    },
    repository::UserRepository,
    Error,
};
use chrono::Utc;
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

// ========== update with stale version -> OptimisticLockConflict ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_stale_version_returns_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = repo.create(&make_user("user_stale")).await.unwrap();
    let original_version = user.version;

    // First update succeeds
    let mut updated_user = user.clone();
    updated_user.username = "user_stale_v1".to_string();
    updated_user.email = Some("user_stale_v1@test.com".to_string());
    let v1 = repo.update(&updated_user, original_version).await.unwrap();
    assert_eq!(v1.version, original_version + 1);

    // Second update with stale version (original_version) -> should get OptimisticLockConflict
    let mut stale_user = user.clone();
    stale_user.username = "user_stale_v2".to_string();
    stale_user.email = Some("user_stale_v2@test.com".to_string());
    let err = repo.update(&stale_user, original_version).await.unwrap_err();
    assert!(
        matches!(err, Error::OptimisticLockConflict),
        "Expected OptimisticLockConflict, got: {:?}", err
    );
}

// ========== update on soft-deleted user -> NotFound ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_soft_deleted_user_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = repo.create(&make_user("user_softdel")).await.unwrap();
    let version = user.version;

    // Soft delete the user
    let deleted = repo.delete(&user.id).await.unwrap();
    assert!(deleted);

    // Trying to update the deleted user should return NotFound (not OptimisticLockConflict)
    let mut updated = user.clone();
    updated.username = "user_softdel_updated".to_string();
    updated.email = Some("user_softdel_updated@test.com".to_string());
    let err = repo.update(&updated, version).await.unwrap_err();
    assert!(
        matches!(err, Error::NotFound(_)),
        "Expected NotFound for soft-deleted user, got: {:?}", err
    );
}

// ========== update_password on deleted user -> NotFound ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_password_deleted_user_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = repo.create(&make_user("user_delpw")).await.unwrap();

    // Soft delete
    repo.delete(&user.id).await.unwrap();

    // Trying to update password on deleted user should return NotFound
    let err = repo.update_password(&user.id, "new_hash").await.unwrap_err();
    assert!(
        matches!(err, Error::NotFound(_)),
        "Expected NotFound for deleted user password update, got: {:?}", err
    );
}

// ========== get_by_ids with mixed existing/soft-deleted IDs ==========

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_mixed_existing_and_deleted() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user1 = repo.create(&make_user("user_mix_1")).await.unwrap();
    let user2 = repo.create(&make_user("user_mix_2")).await.unwrap();
    let user3 = repo.create(&make_user("user_mix_3")).await.unwrap();

    // Soft delete user2
    repo.delete(&user2.id).await.unwrap();

    // Query all three IDs
    let ids = vec![user1.id.clone(), user2.id.clone(), user3.id.clone()];
    let results = repo.get_by_ids(&ids).await.unwrap();

    // Should only return user1 and user3 (user2 is soft-deleted)
    assert_eq!(results.len(), 2);
    let result_ids: Vec<String> = results.iter().map(|u| u.id.as_str().to_string()).collect();
    assert!(result_ids.contains(&user1.id.as_str().to_string()));
    assert!(!result_ids.contains(&user2.id.as_str().to_string()));
    assert!(result_ids.contains(&user3.id.as_str().to_string()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_all_deleted() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user1 = repo.create(&make_user("user_alldel_1")).await.unwrap();
    let user2 = repo.create(&make_user("user_alldel_2")).await.unwrap();

    repo.delete(&user1.id).await.unwrap();
    repo.delete(&user2.id).await.unwrap();

    let ids = vec![user1.id.clone(), user2.id.clone()];
    let results = repo.get_by_ids(&ids).await.unwrap();

    assert!(results.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_empty_input() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let results = repo.get_by_ids(&[]).await.unwrap();
    assert!(results.is_empty());
}
