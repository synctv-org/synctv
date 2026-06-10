//! `UserRepository` integration tests
//!
//! Tests optimistic locking, soft-delete interactions, and batch queries.
//!

use chrono::Utc;
use synctv_core::{
    models::{OpaquePasswordRecord, User, UserId, UserRole, UserStatus},
    repository::{PasswordCredentialMaterial, UserPasswordRepository, UserRepository},
    Error,
};
use synctv_core_testing::{create_test_pool, err, ok};
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
async fn test_update_stale_version_returns_optimistic_lock_conflict() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = ok(
        repo.create(&make_user("user_stale")).await,
        "stale-version test user should be created",
    );
    let original_version = user.version;

    // First update succeeds
    let mut updated_user = user.clone();
    updated_user.username = "user_stale_v1".to_string();
    let v1 = ok(
        repo.update(&updated_user, original_version).await,
        "user update with current version should succeed",
    );
    assert_eq!(v1.version, original_version + 1);

    // Second update with stale version (original_version) -> should get OptimisticLockConflict
    let mut stale_user = user.clone();
    stale_user.username = "user_stale_v2".to_string();
    let err = err(
        repo.update(&stale_user, original_version).await,
        "stale user update should fail",
    );
    assert!(
        matches!(err, Error::OptimisticLockConflict),
        "Expected OptimisticLockConflict, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_soft_deleted_user_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = ok(
        repo.create(&make_user("user_softdel")).await,
        "soft-delete test user should be created",
    );
    let version = user.version;

    // Soft delete the user
    let deleted = ok(repo.delete(&user.id).await, "user should be soft-deleted");
    assert!(deleted);

    // Trying to update the deleted user should return NotFound (not OptimisticLockConflict)
    let mut updated = user.clone();
    updated.username = "user_softdel_updated".to_string();
    let err = err(
        repo.update(&updated, version).await,
        "soft-deleted user update should fail",
    );
    assert!(
        matches!(err, Error::NotFound(_)),
        "Expected NotFound for soft-deleted user, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_password_deleted_user_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user = ok(
        repo.create(&make_user("user_delpw")).await,
        "password deletion test user should be created",
    );

    // Soft delete
    ok(repo.delete(&user.id).await, "user should be soft-deleted");

    let opaque_record = OpaquePasswordRecord {
        record: b"opaque-record".to_vec(),
        credential_identifier: b"synctv:user-id:1".to_vec(),
        ciphersuite: "opaque-ristretto255-sha512-argon2id".to_string(),
        server_setup_version: 1,
    };

    // Trying to update password credentials on deleted user should return NotFound
    let password_repo = UserPasswordRepository::new(pool.clone());
    let err = err(
        password_repo
            .update_with_executor(
                &user.id,
                PasswordCredentialMaterial::opaque_only(&opaque_record),
                &pool,
            )
            .await,
        "deleted user password credential update should fail",
    );
    assert!(
        matches!(err, Error::NotFound(_)),
        "Expected NotFound for deleted user password update, got: {err:?}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_mixed_existing_and_deleted() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user1 = ok(
        repo.create(&make_user("user_mix_1")).await,
        "first mixed query user should be created",
    );
    let user2 = ok(
        repo.create(&make_user("user_mix_2")).await,
        "second mixed query user should be created",
    );
    let user3 = ok(
        repo.create(&make_user("user_mix_3")).await,
        "third mixed query user should be created",
    );

    // Soft delete user2
    ok(
        repo.delete(&user2.id).await,
        "second user should be soft-deleted",
    );

    // Query all three IDs
    let ids = vec![user1.id, user2.id, user3.id];
    let results = ok(
        repo.get_by_ids(&ids).await,
        "mixed user id query should succeed",
    );

    // Should only return user1 and user3 (user2 is soft-deleted)
    assert_eq!(results.len(), 2);
    let result_ids: Vec<String> = results.iter().map(|u| u.id.to_string()).collect();
    assert!(result_ids.contains(&user1.id.to_string()));
    assert!(!result_ids.contains(&user2.id.to_string()));
    assert!(result_ids.contains(&user3.id.to_string()));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_all_deleted() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let user1 = ok(
        repo.create(&make_user("user_alldel_1")).await,
        "first all-deleted query user should be created",
    );
    let user2 = ok(
        repo.create(&make_user("user_alldel_2")).await,
        "second all-deleted query user should be created",
    );

    ok(
        repo.delete(&user1.id).await,
        "first user should be soft-deleted",
    );
    ok(
        repo.delete(&user2.id).await,
        "second user should be soft-deleted",
    );

    let ids = vec![user1.id, user2.id];
    let results = ok(
        repo.get_by_ids(&ids).await,
        "all-deleted user id query should succeed",
    );

    assert!(results.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_ids_empty_input() {
    let (_container, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let results = ok(
        repo.get_by_ids(&[]).await,
        "empty user id query should succeed",
    );
    assert!(results.is_empty());
}
