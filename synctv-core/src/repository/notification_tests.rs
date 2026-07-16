use super::*;
use crate::models::notification::NotificationData;
use crate::models::pagination::PageParams;
use crate::test_helpers::{TestOptionExt, TestResultExt};
use synctv_core_testing::create_test_pool;

fn notification_order_by_sql(query: &NotificationListQuery) -> String {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
    NotificationRepository::push_list_order_by(&mut builder, query);
    builder.sql().as_str().to_string()
}

#[test]
fn test_notification_list_order_by_uses_static_sort_branches() {
    let mut query = NotificationListQuery {
        sort_by: crate::models::NotificationListSortBy::Title,
        sort_direction: crate::models::SortDirection::Asc,
        ..NotificationListQuery::default()
    };
    assert_eq!(
        notification_order_by_sql(&query),
        " ORDER BY title ASC, created_at DESC, id DESC"
    );

    query.sort_by = crate::models::NotificationListSortBy::UpdatedAt;
    query.sort_direction = crate::models::SortDirection::Desc;
    assert_eq!(
        notification_order_by_sql(&query),
        " ORDER BY updated_at DESC, id DESC"
    );

    query.sort_by = crate::models::NotificationListSortBy::CreatedAt;
    query.sort_direction = crate::models::SortDirection::Asc;
    assert_eq!(
        notification_order_by_sql(&query),
        " ORDER BY created_at ASC, id DESC"
    );
}

// Run with: cargo test -p synctv-core notification -- --ignored

/// Test create() creates a notification in the database
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_create_notification() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());

    // Create a user first (for foreign key constraint)
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notification
    let req = CreateNotificationRequest {
        user_id: created_user.id,
        notification_type: NotificationType::SystemAnnouncement,
        title: "Test Notification".to_string(),
        content: "This is a test notification".to_string(),
        data: NotificationData {
            action_url: Some("value".to_string()),
            ..Default::default()
        },
    };

    let notification = repo.create(&req).await.checked("operation should succeed");

    assert!(notification.id > 0);
    assert_eq!(notification.user_id, created_user.id);
    assert_eq!(
        notification.notification_type,
        NotificationType::SystemAnnouncement
    );
    assert_eq!(notification.title, "Test Notification");
    assert_eq!(notification.content, "This is a test notification");
    assert!(!notification.is_read);
}

/// Test get_by_id() retrieves a notification
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());

    // Create user and notification
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    let req = CreateNotificationRequest {
        user_id: created_user.id,
        notification_type: NotificationType::RoomEvent,
        title: "Room Event".to_string(),
        content: "User joined room".to_string(),
        data: NotificationData::default(),
    };
    let created = repo.create(&req).await.checked("operation should succeed");

    // Retrieve by ID
    let found = repo
        .get_by_id(created.id)
        .await
        .checked("operation should succeed");
    assert!(found.is_some());
    let found = found.checked("operation should succeed");
    assert_eq!(found.id, created.id);
    assert_eq!(found.title, "Room Event");
}

/// Test get_by_id() returns None for non-existent notification
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_get_by_id_not_found() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());

    let non_existent_id = i64::MAX;
    let found = repo
        .get_by_id(non_existent_id)
        .await
        .checked("operation should succeed");
    assert!(found.is_none());
}

/// Test list_by_user_with_count() returns paginated notifications
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create multiple notifications
    for i in 0..5 {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: NotificationType::SystemAnnouncement,
            title: format!("Notification {i}"),
            content: format!("Content {i}"),
            data: NotificationData::default(),
        };
        repo.create(&req).await.checked("operation should succeed");
    }

    // List with pagination
    let query = NotificationListQuery {
        pagination: PageParams::new(Some(1), Some(3)),
        is_read: None,
        notification_type: None,
        search: None,
        sort_by: crate::models::NotificationListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
    };

    let (notifications, total) = repo
        .list_by_user_with_count(&created_user.id, &query)
        .await
        .checked("operation should succeed");

    assert_eq!(notifications.len(), 3);
    assert_eq!(total, 5);
}

/// Test list_by_user_with_count() filters by is_read
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count_filter_by_read() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notification
    let req = CreateNotificationRequest {
        user_id: created_user.id,
        notification_type: NotificationType::SystemAnnouncement,
        title: "Test".to_string(),
        content: "Content".to_string(),
        data: NotificationData::default(),
    };
    let notification = repo.create(&req).await.checked("operation should succeed");

    // Mark as read
    repo.mark_as_read(&created_user.id, &[notification.id])
        .await
        .checked("operation should succeed");

    // List only unread
    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: Some(false),
        notification_type: None,
        search: None,
        sort_by: crate::models::NotificationListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
    };
    let (unread, _) = repo
        .list_by_user_with_count(&created_user.id, &query)
        .await
        .checked("operation should succeed");
    assert!(unread.is_empty());

    // List only read
    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: Some(true),
        notification_type: None,
        search: None,
        sort_by: crate::models::NotificationListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
    };
    let (read, _) = repo
        .list_by_user_with_count(&created_user.id, &query)
        .await
        .checked("operation should succeed");
    assert_eq!(read.len(), 1);
}

/// Test list_by_user_with_count() filters by notification_type
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_count_filter_by_type() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notifications of different types
    for nt in [
        NotificationType::SystemAnnouncement,
        NotificationType::RoomInvitation,
    ] {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: nt,
            title: "Test".to_string(),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        repo.create(&req).await.checked("operation should succeed");
    }

    // Filter by SystemAnnouncement
    let query = NotificationListQuery {
        pagination: PageParams::default(),
        is_read: None,
        notification_type: Some(NotificationType::SystemAnnouncement),
        search: None,
        sort_by: crate::models::NotificationListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
    };
    let (notifications, _) = repo
        .list_by_user_with_count(&created_user.id, &query)
        .await
        .checked("operation should succeed");
    assert_eq!(notifications.len(), 1);
    assert_eq!(
        notifications[0].notification_type,
        NotificationType::SystemAnnouncement
    );
}

/// Test mark_as_read() marks notifications as read
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_as_read() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notifications
    let mut notification_ids = Vec::new();
    for i in 0..3 {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: NotificationType::SystemAnnouncement,
            title: format!("Test {i}"),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        let notification = repo.create(&req).await.checked("operation should succeed");
        notification_ids.push(notification.id);
    }

    // Mark first two as read
    let affected = repo
        .mark_as_read(&created_user.id, &notification_ids[..2])
        .await
        .checked("operation should succeed");
    assert_eq!(affected, 2);

    // Verify unread count
    let unread = repo
        .count_unread(&created_user.id)
        .await
        .checked("operation should succeed");
    assert_eq!(unread, 1);
}

/// Test mark_as_read() with empty list returns 0
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_as_read_empty_list() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_id = UserId::new();

    let affected = repo
        .mark_as_read(&user_id, &[])
        .await
        .checked("operation should succeed");
    assert_eq!(affected, 0);
}

/// Test mark_all_as_read() marks all notifications as read
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_all_as_read() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create multiple notifications
    for i in 0..5 {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: NotificationType::SystemAnnouncement,
            title: format!("Test {i}"),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        repo.create(&req).await.checked("operation should succeed");
    }

    // Mark all as read
    let affected = repo
        .mark_all_as_read(&created_user.id, None)
        .await
        .checked("operation should succeed");
    assert_eq!(affected, 5);

    // Verify unread count is 0
    let unread = repo
        .count_unread(&created_user.id)
        .await
        .checked("operation should succeed");
    assert_eq!(unread, 0);
}

/// Test mark_all_as_read() with before timestamp
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_all_as_read_with_before() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notification
    let req = CreateNotificationRequest {
        user_id: created_user.id,
        notification_type: NotificationType::SystemAnnouncement,
        title: "Test".to_string(),
        content: "Content".to_string(),
        data: NotificationData::default(),
    };
    repo.create(&req).await.checked("operation should succeed");

    // Mark all as read before a future time (should mark this one)
    let before = crate::SystemClock.now() + chrono::Duration::days(1);
    let affected = repo
        .mark_all_as_read(&created_user.id, Some(before))
        .await
        .checked("operation should succeed");
    assert_eq!(affected, 1);
}

/// Test delete() removes a notification
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_notification() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notification
    let req = CreateNotificationRequest {
        user_id: created_user.id,
        notification_type: NotificationType::SystemAnnouncement,
        title: "To Delete".to_string(),
        content: "Content".to_string(),
        data: NotificationData::default(),
    };
    let notification = repo.create(&req).await.checked("operation should succeed");

    // Delete notification
    repo.delete(&created_user.id, notification.id)
        .await
        .checked("operation should succeed");

    // Verify it's deleted
    let found = repo
        .get_by_id(notification.id)
        .await
        .checked("operation should succeed");
    assert!(found.is_none());
}

/// Test delete() returns NotFound for non-existent notification
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_notification_not_found() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_id = UserId::new();
    let non_existent_id = i64::MAX;

    let result = repo.delete(&user_id, non_existent_id).await;
    assert!(result.is_err());
    assert!(matches!(
        result.failed("operation should fail"),
        crate::Error::NotFound(_)
    ));
}

/// Test count_unread() returns correct count
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_unread() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notifications
    for i in 0..3 {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: NotificationType::SystemAnnouncement,
            title: format!("Test {i}"),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        repo.create(&req).await.checked("operation should succeed");
    }

    // Count unread
    let unread = repo
        .count_unread(&created_user.id)
        .await
        .checked("operation should succeed");
    assert_eq!(unread, 3);

    // Mark one as read
    let query = NotificationListQuery {
        pagination: PageParams::new(Some(1), Some(1)),
        is_read: Some(false),
        notification_type: None,
        search: None,
        sort_by: crate::models::NotificationListSortBy::CreatedAt,
        sort_direction: crate::models::SortDirection::Desc,
    };
    let (notifications, _) = repo
        .list_by_user_with_count(&created_user.id, &query)
        .await
        .checked("operation should succeed");
    repo.mark_as_read(&created_user.id, &[notifications[0].id])
        .await
        .checked("operation should succeed");

    // Verify count decreased
    let unread = repo
        .count_unread(&created_user.id)
        .await
        .checked("operation should succeed");
    assert_eq!(unread, 2);
}

/// Test count_by_user() with filters
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_user_with_filters() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notifications of different types
    for nt in [
        NotificationType::SystemAnnouncement,
        NotificationType::RoomInvitation,
        NotificationType::RoomEvent,
    ] {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: nt,
            title: "Test".to_string(),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        repo.create(&req).await.checked("operation should succeed");
    }

    // Count by type
    let count = repo
        .count_by_user(
            &created_user.id,
            None,
            Some(&NotificationType::SystemAnnouncement),
        )
        .await
        .checked("operation should succeed");
    assert_eq!(count, 1);

    // Count all
    let count = repo
        .count_by_user(&created_user.id, None, None)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 3);
}

/// Test delete_all_read() removes only read notifications
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_delete_all_read() {
    let (_postgres, pool) = create_test_pool().await;
    let repo = NotificationRepository::new(pool.clone());
    let user_repo = crate::repository::user::UserRepository::new(pool.clone());

    // Create user
    let user = crate::test_helpers::UserFixture::new().build();
    let created_user = user_repo
        .create(&user)
        .await
        .checked("operation should succeed");

    // Create notifications
    let mut ids = Vec::new();
    for i in 0..3 {
        let req = CreateNotificationRequest {
            user_id: created_user.id,
            notification_type: NotificationType::SystemAnnouncement,
            title: format!("Test {i}"),
            content: "Content".to_string(),
            data: NotificationData::default(),
        };
        let notification = repo.create(&req).await.checked("operation should succeed");
        ids.push(notification.id);
    }

    // Mark first as read
    repo.mark_as_read(&created_user.id, &[ids[0]])
        .await
        .checked("operation should succeed");

    // Delete all read
    let affected = repo
        .delete_all_read(&created_user.id)
        .await
        .checked("operation should succeed");
    assert_eq!(affected, 1);

    // Verify remaining count
    let count = repo
        .count_by_user(&created_user.id, None, None)
        .await
        .checked("operation should succeed");
    assert_eq!(count, 2);
}
