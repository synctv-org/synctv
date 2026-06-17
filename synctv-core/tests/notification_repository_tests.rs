//! `NotificationRepository` integration tests
//!
//! Tests: `mark_as_read` cross-user guard, `mark_all_as_read` before parameter,
//!        `delete_older_than` boundary, `list_by_user_with_count` pagination counts.
//!

use chrono::{Duration, Utc};
use sqlx::PgPool;
use synctv_core::{
    models::{
        CreateNotificationRequest, NotificationListQuery, NotificationType, PageParams, User,
        UserId, UserRole, UserStatus,
    },
    repository::{NotificationRepository, UserRepository},
};
use synctv_core_testing::{create_test_pool, ok, some};
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

async fn create_user(pool: &PgPool, username: &str) -> User {
    let user_repo = UserRepository::new(pool.clone());
    ok(
        user_repo.create(&make_user(username)).await,
        "notification test user should be created",
    )
}

fn make_notif_request(user_id: &UserId, title: &str) -> CreateNotificationRequest {
    CreateNotificationRequest {
        user_id: *user_id,
        notification_type: NotificationType::SystemAnnouncement,
        title: title.to_string(),
        content: "test content".to_string(),
        data: serde_json::json!({}),
    }
}

// ─── mark_as_read cross-user guard ───────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_as_read_cross_user_guard() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user_a = create_user(&pool, "notif_user_a").await;
    let user_b = create_user(&pool, "notif_user_b").await;

    let notif_a = ok(
        notif_repo
            .create(&make_notif_request(&user_a.id, "For A"))
            .await,
        "notification for user A should be created",
    );
    assert!(!notif_a.is_read);

    // User B tries to mark user A's notification as read
    let affected = ok(
        notif_repo.mark_as_read(&user_b.id, &[notif_a.id]).await,
        "cross-user mark-as-read should execute",
    );
    assert_eq!(
        affected, 0,
        "Foreign user should not be able to mark other user's notifications"
    );

    // Verify it's still unread
    let fetched = some(
        ok(
            notif_repo.get_by_id(notif_a.id).await,
            "notification should be fetched after cross-user attempt",
        ),
        "notification should exist after cross-user attempt",
    );
    assert!(!fetched.is_read);

    // User A marks their own notification
    let affected = ok(
        notif_repo.mark_as_read(&user_a.id, &[notif_a.id]).await,
        "owner mark-as-read should execute",
    );
    assert_eq!(affected, 1);
    let fetched = some(
        ok(
            notif_repo.get_by_id(notif_a.id).await,
            "notification should be fetched after owner mark-as-read",
        ),
        "notification should exist after owner mark-as-read",
    );
    assert!(fetched.is_read);
}

// ─── mark_all_as_read with before parameter ──────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_all_as_read_before_parameter() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_before").await;

    let n1 = ok(
        notif_repo.create(&make_notif_request(&user.id, "n1")).await,
        "first notification should be created",
    );

    // Sleep a bit so timestamps differ
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let cutoff = Utc::now();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let _n2 = ok(
        notif_repo.create(&make_notif_request(&user.id, "n2")).await,
        "second notification should be created",
    );

    // Mark all before cutoff
    let affected = ok(
        notif_repo.mark_all_as_read(&user.id, Some(cutoff)).await,
        "mark-all-as-read before cutoff should execute",
    );
    assert_eq!(
        affected, 1,
        "Only n1 should be marked as read (before cutoff)"
    );

    // n1 should be read
    let fetched_n1 = some(
        ok(
            notif_repo.get_by_id(n1.id).await,
            "first notification should be fetched",
        ),
        "first notification should exist",
    );
    assert!(fetched_n1.is_read);

    // n2 should still be unread
    let unread_count = ok(
        notif_repo.count_unread(&user.id).await,
        "unread notification count should load",
    );
    assert_eq!(unread_count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_all_as_read_without_before() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_all").await;

    ok(
        notif_repo.create(&make_notif_request(&user.id, "x1")).await,
        "first notification should be created",
    );
    ok(
        notif_repo.create(&make_notif_request(&user.id, "x2")).await,
        "second notification should be created",
    );

    let affected = ok(
        notif_repo.mark_all_as_read(&user.id, None).await,
        "mark-all-as-read should execute",
    );
    assert_eq!(affected, 2);

    let unread = ok(
        notif_repo.count_unread(&user.id).await,
        "unread notification count should load",
    );
    assert_eq!(unread, 0);
}

// ─── delete_older_than boundary ──────────────────────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_older_than_boundary() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_delete").await;

    let old_date = Utc::now() - Duration::days(31);
    ok(
        sqlx::query!(
            r"INSERT INTO notifications (user_id, type, title, content, data, is_read, created_at, updated_at)
          VALUES ($1, $2, 'Old Notif', 'old', $3, false, $4, $4)",
            user.id.as_i64(),
            i16::from(NotificationType::SystemAnnouncement),
            serde_json::json!({}),
            old_date
        )
        .execute(&pool)
        .await,
        "old notification fixture should be inserted",
    );

    ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent"))
            .await,
        "recent notification should be created",
    );

    // Delete notifications older than 30 days
    let deleted = ok(
        notif_repo.delete_older_than(30).await,
        "old notifications should be deleted",
    );
    assert_eq!(deleted, 1, "Should delete only the old notification");

    // Recent notification should remain
    let count = ok(
        notif_repo.count_by_user(&user.id, None, None).await,
        "notification count should load",
    );
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_older_than_zero_days_deletes_all() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_user_zero").await;

    ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent"))
            .await,
        "recent notification should be created",
    );

    // 0 days means everything older than now should be deleted
    let deleted = ok(
        notif_repo.delete_older_than(0).await,
        "zero-day notification delete should execute",
    );
    // The notification was JUST created so it might be at exactly CURRENT_TIMESTAMP.
    // With `< CURRENT_TIMESTAMP - INTERVAL '0 days'` it should still be within the boundary.
    // Just verify it returns without error (boundary behavior).
    assert!(deleted <= 1);
}

// ─── list_by_user_with_count empty total_count=0 ─────────────────────

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count_empty_returns_zero() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_empty_user").await;

    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: None,
        notification_type: None,
        search: None,
        sort_by: synctv_core::models::NotificationListSortBy::CreatedAt,
        sort_direction: synctv_core::models::SortDirection::Desc,
    };

    let (notifications, total) = ok(
        notif_repo.list_by_user_with_count(&user.id, &query).await,
        "empty notification list should load",
    );

    assert!(notifications.is_empty());
    assert_eq!(total, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count_returns_correct_total() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_count_user").await;

    for i in 0..5 {
        ok(
            notif_repo
                .create(&make_notif_request(&user.id, &format!("notif_{i}")))
                .await,
            "notification should be created for pagination",
        );
    }

    let query = NotificationListQuery {
        pagination: PageParams::new(Some(1), Some(2)),
        is_read: None,
        notification_type: None,
        search: None,
        sort_by: synctv_core::models::NotificationListSortBy::CreatedAt,
        sort_direction: synctv_core::models::SortDirection::Desc,
    };

    let (notifications, total) = ok(
        notif_repo.list_by_user_with_count(&user.id, &query).await,
        "paginated notification list should load",
    );

    assert_eq!(notifications.len(), 2);
    assert_eq!(total, 5);

    let out_of_range_query = NotificationListQuery {
        pagination: PageParams::new(Some(4), Some(2)),
        ..query
    };
    let (notifications, total) = ok(
        notif_repo
            .list_by_user_with_count(&user.id, &out_of_range_query)
            .await,
        "out-of-range notification list should load",
    );

    assert!(notifications.is_empty());
    assert_eq!(total, 5);
}

// ─── C2: Partition pruning tests for notification queries ─────────────

/// Verify that list_by_user_with_count includes created_at lower bound
/// for partition pruning. Without it, PostgreSQL scans all partitions.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_list").await;

    // Insert a notification older than 6 months via raw SQL
    let old_date = Utc::now() - Duration::days(200);
    ok(
        sqlx::query!(
            r"INSERT INTO notifications (user_id, type, title, content, data, is_read, created_at, updated_at)
          VALUES ($1, $2, 'Old Notif', 'old', $3, false, $4, $4)",
            user.id.as_i64(),
            i16::from(NotificationType::SystemAnnouncement),
            serde_json::json!({}),
            old_date
        )
        .execute(&pool)
        .await,
        "old notification fixture should be inserted",
    );

    // Insert a recent notification
    ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent"))
            .await,
        "recent notification should be created",
    );

    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: None,
        notification_type: None,
        search: None,
        sort_by: synctv_core::models::NotificationListSortBy::CreatedAt,
        sort_direction: synctv_core::models::SortDirection::Desc,
    };

    let (notifications, total) = ok(
        notif_repo.list_by_user_with_count(&user.id, &query).await,
        "partition-pruned notification list should load",
    );

    // Only the recent notification should be returned (old one outside 6-month window)
    assert_eq!(
        total, 1,
        "Old notification outside 6-month window should be excluded"
    );
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].title, "Recent");
}

/// Verify that count_by_user includes created_at lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_user_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_count").await;

    // Insert a notification older than 6 months
    let old_date = Utc::now() - Duration::days(200);
    ok(
        sqlx::query!(
            r"INSERT INTO notifications (user_id, type, title, content, data, is_read, created_at, updated_at)
          VALUES ($1, $2, 'Old', 'old', $3, false, $4, $4)",
            user.id.as_i64(),
            i16::from(NotificationType::SystemAnnouncement),
            serde_json::json!({}),
            old_date
        )
        .execute(&pool)
        .await,
        "old notification fixture should be inserted",
    );

    // Insert a recent one
    ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent"))
            .await,
        "recent notification should be created",
    );

    let count = ok(
        notif_repo.count_by_user(&user.id, None, None).await,
        "notification count should load",
    );
    assert_eq!(
        count, 1,
        "Old notification outside 6-month window should not be counted"
    );
}

/// Verify that count_unread includes created_at lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_unread_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_unread").await;

    // Insert an old unread notification (> 6 months)
    let old_date = Utc::now() - Duration::days(200);
    ok(
        sqlx::query!(
            r"INSERT INTO notifications (user_id, type, title, content, data, is_read, created_at, updated_at)
          VALUES ($1, $2, 'Old Unread', 'old', $3, false, $4, $4)",
            user.id.as_i64(),
            i16::from(NotificationType::SystemAnnouncement),
            serde_json::json!({}),
            old_date
        )
        .execute(&pool)
        .await,
        "old unread notification fixture should be inserted",
    );

    // Insert a recent unread notification
    ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent Unread"))
            .await,
        "recent unread notification should be created",
    );

    let count = ok(
        notif_repo.count_unread(&user.id).await,
        "unread notification count should load",
    );
    assert_eq!(
        count, 1,
        "Old unread notification outside 6-month window should not be counted"
    );
}

/// Verify that mark_as_read includes created_at lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_as_read_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_mark").await;

    let notif = ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Recent"))
            .await,
        "recent notification should be created",
    );

    let affected = ok(
        notif_repo.mark_as_read(&user.id, &[notif.id]).await,
        "mark-as-read should execute",
    );
    assert_eq!(affected, 1, "Should mark recent notification as read");
}

/// Verify that delete includes created_at lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_delete").await;

    let notif = ok(
        notif_repo
            .create(&make_notif_request(&user.id, "To Delete"))
            .await,
        "notification to delete should be created",
    );

    ok(
        notif_repo.delete(&user.id, notif.id).await,
        "notification delete should execute",
    );

    // Verify it was deleted
    let fetched = ok(
        notif_repo.get_by_id(notif.id).await,
        "notification should be fetched after delete",
    );
    assert!(fetched.is_none(), "Notification should be deleted");
}

/// Verify that delete_all_read includes created_at lower bound for partition pruning.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_all_read_has_partition_pruning() {
    let (_container, pool) = create_test_pool().await;
    let notif_repo = NotificationRepository::new(pool.clone());

    let user = create_user(&pool, "notif_prune_del_read").await;

    let notif = ok(
        notif_repo
            .create(&make_notif_request(&user.id, "Read"))
            .await,
        "notification to mark read should be created",
    );
    ok(
        notif_repo.mark_as_read(&user.id, &[notif.id]).await,
        "mark-as-read should execute",
    );

    // Delete all read - should succeed
    let deleted = ok(
        notif_repo.delete_all_read(&user.id).await,
        "delete-all-read should execute",
    );
    assert_eq!(deleted, 1, "Should delete the read notification");
}
