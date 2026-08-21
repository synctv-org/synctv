use super::*;
use crate::models::SignupMethod;
use crate::test_helpers::TestResultExt;
use synctv_core_testing::create_test_pool;

#[test]
fn test_user_list_order_clause_supports_username_ascending() {
    let query = UserListQuery {
        search: None,
        status: None,
        role: None,
        is_banned: None,
        sort_by: crate::models::UserListSortBy::Username,
        sort_direction: crate::models::SortDirection::Asc,
        pagination: crate::models::PageParams::default(),
        include_deleted: false,
    };

    assert_eq!(UserRepository::order_by_sql(&query), "username ASC, id ASC");
}

#[test]
fn test_user_list_order_clause_supports_effective_status() {
    let query = UserListQuery {
        search: None,
        status: None,
        role: None,
        is_banned: None,
        sort_by: crate::models::UserListSortBy::Status,
        sort_direction: crate::models::SortDirection::Desc,
        pagination: crate::models::PageParams::default(),
        include_deleted: false,
    };

    assert_eq!(
        UserRepository::order_by_sql(&query),
        "is_banned DESC, created_at DESC, id DESC"
    );
}

#[test]
fn test_user_list_order_clause_supports_email_nulls_last() {
    let query = UserListQuery {
        search: None,
        status: None,
        role: None,
        is_banned: None,
        sort_by: crate::models::UserListSortBy::Email,
        sort_direction: crate::models::SortDirection::Asc,
        pagination: crate::models::PageParams::default(),
        include_deleted: false,
    };

    assert_eq!(
        UserRepository::order_by_sql(&query),
        "email ASC NULLS LAST, id ASC"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_filters_by_search_and_role_with_total() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let alpha = repo
        .create(&User::new("zz_list_alpha".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    let beta = repo
        .create(&User::new("zz_list_beta".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    let admin = repo
        .create(&User::new("zz_list_admin".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    repo.update_role(&admin.id, UserRole::Admin, admin.version)
        .await
        .checked("operation should succeed");

    let query = UserListQuery {
        search: Some("zz_list_".to_string()),
        status: Some(UserStatus::Active),
        role: Some(UserRole::User),
        is_banned: None,
        sort_by: crate::models::UserListSortBy::Username,
        sort_direction: crate::models::SortDirection::Asc,
        pagination: crate::models::PageParams::new(Some(1), Some(10)),
        include_deleted: false,
    };
    let (users, total) = repo.list(&query).await.checked("operation should succeed");
    let usernames: Vec<_> = users.iter().map(|user| user.username.as_str()).collect();

    assert_eq!(total, 2);
    assert_eq!(
        usernames,
        vec![alpha.username.as_str(), beta.username.as_str()]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_admins_returns_root_and_admin_only() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());

    let root = repo
        .create(&User::new("zz_admin_root".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    let admin = repo
        .create(&User::new("zz_admin_admin".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    repo.create(&User::new("zz_admin_user".into(), SignupMethod::Email))
        .await
        .checked("operation should succeed");
    let root = repo
        .update_role(&root.id, UserRole::Root, root.version)
        .await
        .checked("operation should succeed");
    let admin = repo
        .update_role(&admin.id, UserRole::Admin, admin.version)
        .await
        .checked("operation should succeed");

    let query = UserListQuery {
        search: Some("zz_admin_".to_string()),
        status: None,
        role: None,
        is_banned: None,
        sort_by: crate::models::UserListSortBy::Username,
        sort_direction: crate::models::SortDirection::Asc,
        pagination: crate::models::PageParams::new(Some(1), Some(10)),
        include_deleted: false,
    };
    let (users, total) = repo
        .list_admins(&query)
        .await
        .checked("operation should succeed");
    let usernames: Vec<_> = users.iter().map(|user| user.username.as_str()).collect();

    assert_eq!(total, 2);
    assert_eq!(
        usernames,
        vec![admin.username.as_str(), root.username.as_str()]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());
    let user = User::new("testuser".into(), SignupMethod::Email);
    let created = repo.create(&user).await.checked("operation should succeed");
    assert_eq!(created.username, "testuser");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_oauth2_user_without_password_skips_password_credentials() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());
    let user = User::new_with_status(
        "oauth_without_password".into(),
        SignupMethod::OAuth2,
        crate::models::UserStatus::Active,
    );

    let created = repo
        .create(&user)
        .await
        .checked("OAuth2 user should be created without password credentials");
    let credential_exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1
            FROM auth_password_credentials
            WHERE user_id = $1
        )
        "#,
        created.id.as_i64(),
    )
    .fetch_one(&pool)
    .await
    .checked("credential existence query should succeed");

    assert!(!credential_exists.unwrap_or(false));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_user_duplicate_username_returns_already_exists() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());
    let user1 = User::new("same_name".into(), SignupMethod::Email);
    repo.create(&user1)
        .await
        .checked("operation should succeed");
    let user2 = User::new("same_name".into(), SignupMethod::Email);
    let err = repo.create(&user2).await.failed("operation should fail");
    assert!(matches!(err, Error::AlreadyExists(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_soft_delete_user() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());
    let user = User::new("deleteme".into(), SignupMethod::Email);
    let created = repo.create(&user).await.checked("operation should succeed");
    assert!(repo
        .delete(&created.id)
        .await
        .checked("operation should succeed"));
    // Soft-deleted users should not be returned by get_by_id
    assert!(repo
        .get_by_id(&created.id)
        .await
        .checked("operation should succeed")
        .is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_user_blocking_is_personal_idempotent_and_searchable() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = UserRepository::new(pool.clone());
    let blocker = repo
        .create(&User::new("blocker".into(), SignupMethod::Email))
        .await
        .checked("blocker should be created");
    let blocked_alpha = repo
        .create(&User::new("blocked_alpha".into(), SignupMethod::Email))
        .await
        .checked("first blocked user should be created");
    let blocked_beta = repo
        .create(&User::new("blocked_beta".into(), SignupMethod::Email))
        .await
        .checked("second blocked user should be created");

    let first_blocked_at = repo
        .block_user(&blocker.id, &blocked_alpha.id)
        .await
        .checked("user should be blocked");
    let repeated_blocked_at = repo
        .block_user(&blocker.id, &blocked_alpha.id)
        .await
        .checked("blocking should be idempotent");
    repo.block_user(&blocker.id, &blocked_beta.id)
        .await
        .checked("second user should be blocked");

    assert_eq!(repeated_blocked_at, first_blocked_at);
    assert!(repo
        .is_blocking(&blocker.id, &blocked_alpha.id)
        .await
        .checked("blocking relationship should be readable"));
    assert!(!repo
        .is_blocking(&blocked_alpha.id, &blocker.id)
        .await
        .checked("reverse blocking relationship should be readable"));

    let (first_page, total) = repo
        .list_blocked_users(
            &blocker.id,
            PageParams::new(Some(1), Some(1)),
            Some("blocked_"),
        )
        .await
        .checked("blocked users should be listed");
    assert_eq!(total, 2);
    assert_eq!(first_page.len(), 1);

    let (search_result, search_total) = repo
        .list_blocked_users(
            &blocker.id,
            PageParams::new(Some(1), Some(10)),
            Some("alpha"),
        )
        .await
        .checked("blocked users should be searchable");
    assert_eq!(search_total, 1);
    assert_eq!(search_result[0].user.id, blocked_alpha.id);

    assert!(repo
        .unblock_user(&blocker.id, &blocked_alpha.id)
        .await
        .checked("user should be unblocked"));
    assert!(!repo
        .unblock_user(&blocker.id, &blocked_alpha.id)
        .await
        .checked("unblocking should be idempotent"));
    assert!(!repo
        .is_blocking(&blocker.id, &blocked_alpha.id)
        .await
        .checked("removed relationship should remain readable"));
}
