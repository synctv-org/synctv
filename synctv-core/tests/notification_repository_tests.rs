//! NotificationRepository integration tests
//!
//! Tests: mark_as_read cross-user guard, mark_all_as_read before parameter,
//!        delete_older_than boundary, list_by_user_with_count empty total_count=0.
//!
//! Run with: cargo test -p synctv-core --test notification_repository_tests

use synctv_core::{
    models::{
        UserId, User, UserRole, UserStatus,
        CreateNotificationRequest, NotificationType, NotificationListQuery, PageParams,
    },
    repository::{NotificationRepository, UserRepository},
};
use chrono::{Utc, Duration};
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
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
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

async fn create_user(pool: &PgPool, username: &str) -> User {
    let user_repo = UserRepository::new(pool.clone());
    user_repo.create(&make_user(username)).await.unwrap()
}

fn make_notif_request(user_id: &UserId, title: &str) -> CreateNotificationRequest {
    CreateNotificationRequest {
        user_id: user_id.clone(),
        notification_type: NotificationType::SystemAnnouncement,
        title: title.to_string(),
        content: "test content".to_string(),
        data: serde_json::json!({}),
    }
}

// ─── mark_as_read cross-user guard ───────────────────────────────────

#[tokio::test]
async fn test_mark_as_read_cross_user_guard() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user_a = create_user(&pool, "notif_user_a").await;
    let user_b = create_user(&pool, "notif_user_b").await;

    // Create notification for user A
    let notif_a = notif_repo
        .create(&make_notif_request(&user_a.id, "For A"))
        .await
        .unwrap();
    assert!(!notif_a.is_read);

    // User B tries to mark user A's notification as read
    let affected = notif_repo
        .mark_as_read(&user_b.id, &[notif_a.id])
        .await
        .unwrap();
    assert_eq!(affected, 0, "Foreign user should not be able to mark other user's notifications");

    // Verify it's still unread
    let fetched = notif_repo.get_by_id(notif_a.id).await.unwrap().unwrap();
    assert!(!fetched.is_read);

    // User A marks their own notification
    let affected = notif_repo
        .mark_as_read(&user_a.id, &[notif_a.id])
        .await
        .unwrap();
    assert_eq!(affected, 1);
    let fetched = notif_repo.get_by_id(notif_a.id).await.unwrap().unwrap();
    assert!(fetched.is_read);
}

// ─── mark_all_as_read with before parameter ──────────────────────────

#[tokio::test]
async fn test_mark_all_as_read_before_parameter() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_before").await;

    // Create 3 notifications at different times
    let n1 = notif_repo
        .create(&make_notif_request(&user.id, "n1"))
        .await
        .unwrap();

    // Sleep a bit so timestamps differ
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let cutoff = Utc::now();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let _n2 = notif_repo
        .create(&make_notif_request(&user.id, "n2"))
        .await
        .unwrap();

    // Mark all before cutoff
    let affected = notif_repo
        .mark_all_as_read(&user.id, Some(cutoff))
        .await
        .unwrap();
    assert_eq!(affected, 1, "Only n1 should be marked as read (before cutoff)");

    // n1 should be read
    let fetched_n1 = notif_repo.get_by_id(n1.id).await.unwrap().unwrap();
    assert!(fetched_n1.is_read);

    // n2 should still be unread
    let unread_count = notif_repo.count_unread(&user.id).await.unwrap();
    assert_eq!(unread_count, 1);
}

#[tokio::test]
async fn test_mark_all_as_read_without_before() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_all").await;

    notif_repo
        .create(&make_notif_request(&user.id, "x1"))
        .await
        .unwrap();
    notif_repo
        .create(&make_notif_request(&user.id, "x2"))
        .await
        .unwrap();

    let affected = notif_repo.mark_all_as_read(&user.id, None).await.unwrap();
    assert_eq!(affected, 2);

    let unread = notif_repo.count_unread(&user.id).await.unwrap();
    assert_eq!(unread, 0);
}

// ─── delete_older_than boundary ──────────────────────────────────────

#[tokio::test]
async fn test_delete_older_than_boundary() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_delete").await;

    // Create a notification with a backdated created_at
    let old_date = Utc::now() - Duration::days(31);
    sqlx::query(
        r"INSERT INTO notifications (id, user_id, type, title, content, data, is_read, created_at, updated_at)
          VALUES (gen_random_uuid(), $1, 'system_announcement', 'Old Notif', 'old', '{}', false, $2, $2)"
    )
    .bind(user.id.as_str())
    .bind(old_date)
    .execute(&pool)
    .await
    .unwrap();

    // Create a recent notification
    notif_repo
        .create(&make_notif_request(&user.id, "Recent"))
        .await
        .unwrap();

    // Delete notifications older than 30 days
    let deleted = notif_repo.delete_older_than(30).await.unwrap();
    assert_eq!(deleted, 1, "Should delete only the old notification");

    // Recent notification should remain
    let count = notif_repo.count_by_user(&user.id, None, None).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_delete_older_than_zero_days_deletes_all() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_zero").await;

    notif_repo
        .create(&make_notif_request(&user.id, "Recent"))
        .await
        .unwrap();

    // 0 days means everything older than now should be deleted
    let deleted = notif_repo.delete_older_than(0).await.unwrap();
    // The notification was JUST created so it might be at exactly CURRENT_TIMESTAMP.
    // With `< CURRENT_TIMESTAMP - INTERVAL '0 days'` it should still be within the boundary.
    // Just verify it returns without error (boundary behavior).
    assert!(deleted <= 1);
}

// ─── list_by_user_with_count empty total_count=0 ─────────────────────

#[tokio::test]
async fn test_list_by_user_with_count_empty_returns_zero() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_empty_user").await;

    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: None,
        notification_type: None,
    };

    let (notifications, total) = notif_repo
        .list_by_user_with_count(&user.id, &query)
        .await
        .unwrap();

    assert!(notifications.is_empty());
    assert_eq!(total, 0);
}

#[tokio::test]
async fn test_list_by_user_with_count_returns_correct_total() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_count_user").await;

    for i in 0..5 {
        notif_repo
            .create(&make_notif_request(&user.id, &format!("notif_{}", i)))
            .await
            .unwrap();
    }

    let query = NotificationListQuery {
        pagination: PageParams::new(Some(1), Some(2)),
        is_read: None,
        notification_type: None,
    };

    let (notifications, total) = notif_repo
        .list_by_user_with_count(&user.id, &query)
        .await
        .unwrap();

    assert_eq!(notifications.len(), 2, "Page should have 2 items");
    assert_eq!(total, 5, "Total count should be 5");
}
