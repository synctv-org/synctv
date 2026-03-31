use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Row};
use uuid::Uuid;

use crate::{
    models::{
        id::UserId,
        notification::{
            CreateNotificationRequest, Notification, NotificationListQuery, NotificationType,
        },
    },
    Error, Result,
};

/// Notification repository for database operations
#[derive(Clone, Debug)]
pub struct NotificationRepository {
    pool: PgPool,
}

impl NotificationRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new notification
    pub async fn create(&self, req: &CreateNotificationRequest) -> Result<Notification> {
        let id = Uuid::new_v4();
        let now = Utc::now();

        let n = sqlx::query_as::<_, Notification>(
            r"
            INSERT INTO notifications (id, user_id, type, title, content, data, is_read, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, user_id, type, title, content, data, is_read, created_at, updated_at
            ",
        )
        .bind(id)
        .bind(req.user_id.as_str())
        .bind(req.notification_type.to_string())
        .bind(&req.title)
        .bind(&req.content)
        .bind(&req.data)
        .bind(false)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(n)
    }

    /// Get notification by ID
    ///
    /// The notifications table is partitioned by `created_at`. Querying by `id`
    /// alone forces `PostgreSQL` to scan every partition. Adding a `created_at`
    /// range filter (last year to now) lets the planner prune irrelevant
    /// partitions and use a targeted index scan instead.
    pub async fn get_by_id(&self, notification_id: Uuid) -> Result<Option<Notification>> {
        let now = Utc::now();
        let one_year_ago = now - chrono::Duration::days(365);

        let n = sqlx::query_as::<_, Notification>(
            r"
            SELECT id, user_id, type, title, content, data, is_read, created_at, updated_at
            FROM notifications
            WHERE id = $1
              AND created_at >= $2
              AND created_at <= $3
            ",
        )
        .bind(notification_id)
        .bind(one_year_ago)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        Ok(n)
    }

    /// List notifications for a user with pagination, filters, and total count.
    ///
    /// Uses `COUNT(*) OVER()` window function to return both the list and total count
    /// in a single query, avoiding a separate count round trip.
    /// Dynamic query building eliminates the need for separate query variants per filter combo.
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn list_by_user_with_count(
        &self,
        user_id: &UserId,
        query: &NotificationListQuery,
    ) -> Result<(Vec<Notification>, i64)> {
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;

        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, user_id, type, title, content, data, is_read, created_at, updated_at, \
             COUNT(*) OVER() AS total_count \
             FROM notifications WHERE user_id = ",
        );
        qb.push_bind(user_id.as_str());

        // Partition pruning: limit scan to the retention window (6 months)
        qb.push(" AND created_at >= NOW() - INTERVAL '6 months'");

        if let Some(notification_type) = &query.notification_type {
            qb.push(" AND type = ");
            qb.push_bind(notification_type.to_string());
        }
        if let Some(is_read) = query.is_read {
            qb.push(" AND is_read = ");
            qb.push_bind(is_read);
        }

        qb.push(" ORDER BY created_at DESC LIMIT ");
        qb.push_bind(limit);
        qb.push(" OFFSET ");
        qb.push_bind(offset);

        let rows = qb.build().fetch_all(&self.pool).await?;

        let total = rows
            .first()
            .map_or(0i64, |row| row.try_get("total_count").unwrap_or(0));
        let notifications: Result<Vec<Notification>> = rows
            .into_iter()
            .map(|row| Ok(Notification::from_row(&row)?))
            .collect();

        Ok((notifications?, total))
    }

    /// Count notifications for a user (for pagination)
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn count_by_user(
        &self,
        user_id: &UserId,
        is_read: Option<bool>,
        notification_type: Option<&NotificationType>,
    ) -> Result<i64> {
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> =
            sqlx::QueryBuilder::new("SELECT COUNT(*) FROM notifications WHERE user_id = ");
        qb.push_bind(user_id.as_str());

        // Partition pruning: limit scan to the retention window (6 months)
        qb.push(" AND created_at >= NOW() - INTERVAL '6 months'");

        if let Some(notification_type) = notification_type {
            qb.push(" AND type = ");
            qb.push_bind(notification_type.to_string());
        }
        if let Some(is_read) = is_read {
            qb.push(" AND is_read = ");
            qb.push_bind(is_read);
        }

        let count: i64 = qb.build_query_scalar().fetch_one(&self.pool).await?;

        Ok(count)
    }

    /// Get unread count for a user
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn count_unread(&self, user_id: &UserId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*)
            FROM notifications
            WHERE user_id = $1 AND is_read = FALSE
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
        )
        .bind(user_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Mark notifications as read
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn mark_as_read(&self, user_id: &UserId, notification_ids: &[Uuid]) -> Result<u64> {
        if notification_ids.is_empty() {
            return Ok(0);
        }

        let result = sqlx::query(
            r"
            UPDATE notifications
            SET is_read = TRUE, updated_at = NOW()
            WHERE user_id = $1 AND id = ANY($2)
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
        )
        .bind(user_id.as_str())
        .bind(notification_ids)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Mark all notifications as read before a certain time (or all if no time specified)
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn mark_all_as_read(
        &self,
        user_id: &UserId,
        before: Option<DateTime<Utc>>,
    ) -> Result<u64> {
        let result = if let Some(before_time) = before {
            sqlx::query(
                r"
                UPDATE notifications
                SET is_read = TRUE, updated_at = NOW()
                WHERE user_id = $1 AND is_read = FALSE AND created_at <= $2
                  AND created_at >= NOW() - INTERVAL '6 months'
                ",
            )
            .bind(user_id.as_str())
            .bind(before_time)
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query(
                r"
                UPDATE notifications
                SET is_read = TRUE, updated_at = NOW()
                WHERE user_id = $1 AND is_read = FALSE
                  AND created_at >= NOW() - INTERVAL '6 months'
                ",
            )
            .bind(user_id.as_str())
            .execute(&self.pool)
            .await?
        };

        Ok(result.rows_affected())
    }

    /// Delete a notification
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    /// Notifications older than 6 months are purged by `delete_older_than`,
    /// so this time bound is safe for user-facing delete operations.
    pub async fn delete(&self, user_id: &UserId, notification_id: Uuid) -> Result<()> {
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND id = $2
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
        )
        .bind(user_id.as_str())
        .bind(notification_id)
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound("Notification not found".to_string()));
        }

        Ok(())
    }

    /// Delete all read notifications for a user
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND is_read = TRUE
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
        )
        .bind(user_id.as_str())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete all notifications older than the specified number of days,
    /// regardless of read status. Prevents unbounded table growth from
    /// unread notifications that are never acknowledged.
    pub async fn delete_older_than(&self, days: i32) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
        )
        .bind(days)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core_testing::create_test_pool;
    use crate::models::notification::{MarkAllAsReadRequest, MarkAsReadRequest};
    use crate::models::pagination::PageParams;

    // ========== Unit Tests (No Database Required) ==========

    /// Test CreateNotificationRequest struct creation with minimal fields
    #[test]
    fn test_create_notification_request_minimal() {
        let user_id = UserId::from_string("user_123".to_string());
        let req = CreateNotificationRequest {
            user_id,
            notification_type: NotificationType::SystemAnnouncement,
            title: "Test Title".to_string(),
            content: "Test Content".to_string(),
            data: serde_json::json!({}),
        };

        assert_eq!(req.user_id.as_str(), "user_123");
        assert_eq!(req.notification_type, NotificationType::SystemAnnouncement);
        assert_eq!(req.title, "Test Title");
        assert_eq!(req.content, "Test Content");
        assert_eq!(req.data, serde_json::json!({}));
    }

    /// Test CreateNotificationRequest with custom data
    #[test]
    fn test_create_notification_request_with_data() {
        let user_id = UserId::from_string("user_456".to_string());
        let data = serde_json::json!({
            "room_id": "room_abc",
            "inviter": "user_789"
        });

        let req = CreateNotificationRequest {
            user_id,
            notification_type: NotificationType::RoomInvitation,
            title: "Room Invitation".to_string(),
            content: "You have been invited".to_string(),
            data,
        };

        assert_eq!(req.data["room_id"], "room_abc");
        assert_eq!(req.data["inviter"], "user_789");
    }

    /// Test NotificationListQuery default pagination
    #[test]
    fn test_notification_list_query_default_pagination() {
        let query = NotificationListQuery {
            pagination: PageParams::default(),
            is_read: None,
            notification_type: None,
        };

        assert_eq!(query.pagination.page, 1);
        assert_eq!(query.pagination.page_size, 20);
        assert!(query.is_read.is_none());
        assert!(query.notification_type.is_none());
    }

    /// Test NotificationListQuery with filters
    #[test]
    fn test_notification_list_query_with_filters() {
        let query = NotificationListQuery {
            pagination: PageParams::new(Some(2), Some(10)),
            is_read: Some(false),
            notification_type: Some(NotificationType::RoomEvent),
        };

        assert_eq!(query.pagination.page, 2);
        assert_eq!(query.pagination.page_size, 10);
        assert_eq!(query.is_read, Some(false));
        assert_eq!(query.notification_type, Some(NotificationType::RoomEvent));
    }

    /// Test MarkAsReadRequest with multiple IDs
    #[test]
    fn test_mark_as_read_request() {
        let req = MarkAsReadRequest {
            notification_ids: vec![
                Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
                Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap(),
            ],
        };

        assert_eq!(req.notification_ids.len(), 2);
    }

    /// Test MarkAsReadRequest with empty list
    #[test]
    fn test_mark_as_read_request_empty() {
        let req = MarkAsReadRequest {
            notification_ids: vec![],
        };

        assert!(req.notification_ids.is_empty());
    }

    /// Test MarkAllAsReadRequest without before timestamp
    #[test]
    fn test_mark_all_as_read_request_no_before() {
        let req = MarkAllAsReadRequest { before: None };

        assert!(req.before.is_none());
    }

    /// Test MarkAllAsReadRequest with before timestamp
    #[test]
    fn test_mark_all_as_read_request_with_before() {
        let before = chrono::Utc::now();
        let req = MarkAllAsReadRequest {
            before: Some(before),
        };

        assert!(req.before.is_some());
    }

    /// Test NotificationType variants
    #[test]
    fn test_notification_type_variants() {
        assert_eq!(
            NotificationType::RoomInvitation.to_string(),
            "room_invitation"
        );
        assert_eq!(
            NotificationType::SystemAnnouncement.to_string(),
            "system_announcement"
        );
        assert_eq!(NotificationType::RoomEvent.to_string(), "room_event");
        assert_eq!(
            NotificationType::PasswordReset.to_string(),
            "password_reset"
        );
        assert_eq!(
            NotificationType::EmailVerification.to_string(),
            "email_verification"
        );
    }

    /// Test NotificationType parsing
    #[test]
    fn test_notification_type_parsing() {
        assert_eq!(
            "room_invitation".parse::<NotificationType>().unwrap(),
            NotificationType::RoomInvitation
        );
        assert_eq!(
            "system_announcement".parse::<NotificationType>().unwrap(),
            NotificationType::SystemAnnouncement
        );
        assert_eq!(
            "room_event".parse::<NotificationType>().unwrap(),
            NotificationType::RoomEvent
        );
    }

    /// Test NotificationType invalid parsing
    #[test]
    fn test_notification_type_invalid_parsing() {
        let result = "invalid_type".parse::<NotificationType>();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid notification type"));
    }

    /// Test NotificationType serde roundtrip
    #[test]
    fn test_notification_type_serde_roundtrip() {
        let types = vec![
            NotificationType::RoomInvitation,
            NotificationType::SystemAnnouncement,
            NotificationType::RoomEvent,
            NotificationType::PasswordReset,
            NotificationType::EmailVerification,
        ];

        for nt in types {
            let json = serde_json::to_string(&nt).unwrap();
            let parsed: NotificationType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, nt);
        }
    }

    // ========== Integration Tests (Require Docker) ==========
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notification
        let req = CreateNotificationRequest {
            user_id: created_user.id.clone(),
            notification_type: NotificationType::SystemAnnouncement,
            title: "Test Notification".to_string(),
            content: "This is a test notification".to_string(),
            data: serde_json::json!({"key": "value"}),
        };

        let notification = repo.create(&req).await.unwrap();

        assert!(!notification.id.is_nil());
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
        let created_user = user_repo.create(&user).await.unwrap();

        let req = CreateNotificationRequest {
            user_id: created_user.id.clone(),
            notification_type: NotificationType::RoomEvent,
            title: "Room Event".to_string(),
            content: "User joined room".to_string(),
            data: serde_json::json!({}),
        };
        let created = repo.create(&req).await.unwrap();

        // Retrieve by ID
        let found = repo.get_by_id(created.id).await.unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.title, "Room Event");
    }

    /// Test get_by_id() returns None for non-existent notification
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_by_id_not_found() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = NotificationRepository::new(pool.clone());

        let non_existent_id = Uuid::new_v4();
        let found = repo.get_by_id(non_existent_id).await.unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create multiple notifications
        for i in 0..5 {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: format!("Notification {i}"),
                content: format!("Content {i}"),
                data: serde_json::json!({}),
            };
            repo.create(&req).await.unwrap();
        }

        // List with pagination
        let query = NotificationListQuery {
            pagination: PageParams::new(Some(1), Some(3)),
            is_read: None,
            notification_type: None,
        };

        let (notifications, total) = repo
            .list_by_user_with_count(&created_user.id, &query)
            .await
            .unwrap();

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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notification
        let req = CreateNotificationRequest {
            user_id: created_user.id.clone(),
            notification_type: NotificationType::SystemAnnouncement,
            title: "Test".to_string(),
            content: "Content".to_string(),
            data: serde_json::json!({}),
        };
        let notification = repo.create(&req).await.unwrap();

        // Mark as read
        repo.mark_as_read(&created_user.id, &[notification.id])
            .await
            .unwrap();

        // List only unread
        let query = NotificationListQuery {
            pagination: PageParams::default(),
            is_read: Some(false),
            notification_type: None,
        };
        let (unread, _) = repo
            .list_by_user_with_count(&created_user.id, &query)
            .await
            .unwrap();
        assert!(unread.is_empty());

        // List only read
        let query = NotificationListQuery {
            pagination: PageParams::default(),
            is_read: Some(true),
            notification_type: None,
        };
        let (read, _) = repo
            .list_by_user_with_count(&created_user.id, &query)
            .await
            .unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notifications of different types
        for nt in [
            NotificationType::SystemAnnouncement,
            NotificationType::RoomInvitation,
        ] {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: nt,
                title: "Test".to_string(),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            repo.create(&req).await.unwrap();
        }

        // Filter by SystemAnnouncement
        let query = NotificationListQuery {
            pagination: PageParams::default(),
            is_read: None,
            notification_type: Some(NotificationType::SystemAnnouncement),
        };
        let (notifications, _) = repo
            .list_by_user_with_count(&created_user.id, &query)
            .await
            .unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notifications
        let mut notification_ids = Vec::new();
        for i in 0..3 {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: format!("Test {i}"),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            let notification = repo.create(&req).await.unwrap();
            notification_ids.push(notification.id);
        }

        // Mark first two as read
        let affected = repo
            .mark_as_read(&created_user.id, &notification_ids[..2])
            .await
            .unwrap();
        assert_eq!(affected, 2);

        // Verify unread count
        let unread = repo.count_unread(&created_user.id).await.unwrap();
        assert_eq!(unread, 1);
    }

    /// Test mark_as_read() with empty list returns 0
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_mark_as_read_empty_list() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = NotificationRepository::new(pool.clone());
        let user_id = UserId::new();

        let affected = repo.mark_as_read(&user_id, &[]).await.unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create multiple notifications
        for i in 0..5 {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: format!("Test {i}"),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            repo.create(&req).await.unwrap();
        }

        // Mark all as read
        let affected = repo.mark_all_as_read(&created_user.id, None).await.unwrap();
        assert_eq!(affected, 5);

        // Verify unread count is 0
        let unread = repo.count_unread(&created_user.id).await.unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notification
        let req = CreateNotificationRequest {
            user_id: created_user.id.clone(),
            notification_type: NotificationType::SystemAnnouncement,
            title: "Test".to_string(),
            content: "Content".to_string(),
            data: serde_json::json!({}),
        };
        repo.create(&req).await.unwrap();

        // Mark all as read before a future time (should mark this one)
        let before = chrono::Utc::now() + chrono::Duration::days(1);
        let affected = repo
            .mark_all_as_read(&created_user.id, Some(before))
            .await
            .unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notification
        let req = CreateNotificationRequest {
            user_id: created_user.id.clone(),
            notification_type: NotificationType::SystemAnnouncement,
            title: "To Delete".to_string(),
            content: "Content".to_string(),
            data: serde_json::json!({}),
        };
        let notification = repo.create(&req).await.unwrap();

        // Delete notification
        repo.delete(&created_user.id, notification.id)
            .await
            .unwrap();

        // Verify it's deleted
        let found = repo.get_by_id(notification.id).await.unwrap();
        assert!(found.is_none());
    }

    /// Test delete() returns NotFound for non-existent notification
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_notification_not_found() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = NotificationRepository::new(pool.clone());
        let user_id = UserId::new();
        let non_existent_id = Uuid::new_v4();

        let result = repo.delete(&user_id, non_existent_id).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), crate::Error::NotFound(_)));
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notifications
        for i in 0..3 {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: format!("Test {i}"),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            repo.create(&req).await.unwrap();
        }

        // Count unread
        let unread = repo.count_unread(&created_user.id).await.unwrap();
        assert_eq!(unread, 3);

        // Mark one as read
        let query = NotificationListQuery {
            pagination: PageParams::new(Some(1), Some(1)),
            is_read: Some(false),
            notification_type: None,
        };
        let (notifications, _) = repo
            .list_by_user_with_count(&created_user.id, &query)
            .await
            .unwrap();
        repo.mark_as_read(&created_user.id, &[notifications[0].id])
            .await
            .unwrap();

        // Verify count decreased
        let unread = repo.count_unread(&created_user.id).await.unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notifications of different types
        for nt in [
            NotificationType::SystemAnnouncement,
            NotificationType::RoomInvitation,
            NotificationType::RoomEvent,
        ] {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: nt,
                title: "Test".to_string(),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            repo.create(&req).await.unwrap();
        }

        // Count by type
        let count = repo
            .count_by_user(
                &created_user.id,
                None,
                Some(&NotificationType::SystemAnnouncement),
            )
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Count all
        let count = repo
            .count_by_user(&created_user.id, None, None)
            .await
            .unwrap();
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
        let created_user = user_repo.create(&user).await.unwrap();

        // Create notifications
        let mut ids = Vec::new();
        for i in 0..3 {
            let req = CreateNotificationRequest {
                user_id: created_user.id.clone(),
                notification_type: NotificationType::SystemAnnouncement,
                title: format!("Test {i}"),
                content: "Content".to_string(),
                data: serde_json::json!({}),
            };
            let notification = repo.create(&req).await.unwrap();
            ids.push(notification.id);
        }

        // Mark first as read
        repo.mark_as_read(&created_user.id, &[ids[0]])
            .await
            .unwrap();

        // Delete all read
        let affected = repo.delete_all_read(&created_user.id).await.unwrap();
        assert_eq!(affected, 1);

        // Verify remaining count
        let count = repo
            .count_by_user(&created_user.id, None, None)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    /// Test delete_older_than() removes old notifications
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_delete_older_than() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = NotificationRepository::new(pool.clone());

        // delete_older_than removes notifications older than specified days
        // Since we can't create notifications with past timestamps easily,
        // we just verify the function executes without error
        let affected = repo.delete_older_than(365).await.unwrap();
        // Should be 0 since all test notifications are recent
        assert_eq!(affected, 0);
    }
}
