use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, FromRow};
use uuid::Uuid;

use crate::{
    models::{
        id::UserId,
        notification::{CreateNotificationRequest, Notification, NotificationListQuery, NotificationType},
    },
    Error,
    Result,
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
             FROM notifications WHERE user_id = "
        );
        qb.push_bind(user_id.as_str());

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

        let total = rows.first().map_or(0i64, |row| row.try_get("total_count").unwrap_or(0));
        let notifications: Result<Vec<Notification>> = rows.into_iter()
            .map(|row| Ok(Notification::from_row(&row)?))
            .collect();

        Ok((notifications?, total))
    }

    /// Count notifications for a user (for pagination)
    pub async fn count_by_user(
        &self,
        user_id: &UserId,
        is_read: Option<bool>,
        notification_type: Option<&NotificationType>,
    ) -> Result<i64> {
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT COUNT(*) FROM notifications WHERE user_id = "
        );
        qb.push_bind(user_id.as_str());

        if let Some(notification_type) = notification_type {
            qb.push(" AND type = ");
            qb.push_bind(notification_type.to_string());
        }
        if let Some(is_read) = is_read {
            qb.push(" AND is_read = ");
            qb.push_bind(is_read);
        }

        let count: i64 = qb.build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        Ok(count)
    }

    /// Get unread count for a user
    pub async fn count_unread(&self, user_id: &UserId) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*)
            FROM notifications
            WHERE user_id = $1 AND is_read = FALSE
            ",
        )
        .bind(user_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count)
    }

    /// Mark notifications as read
    pub async fn mark_as_read(&self, user_id: &UserId, notification_ids: &[Uuid]) -> Result<u64> {
        if notification_ids.is_empty() {
            return Ok(0);
        }

        let result = sqlx::query(
            r"
            UPDATE notifications
            SET is_read = TRUE, updated_at = NOW()
            WHERE user_id = $1 AND id = ANY($2)
            ",
        )
        .bind(user_id.as_str())
        .bind(notification_ids)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Mark all notifications as read before a certain time (or all if no time specified)
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
                ",
            )
            .bind(user_id.as_str())
            .execute(&self.pool)
            .await?
        };

        Ok(result.rows_affected())
    }

    /// Delete a notification
    pub async fn delete(&self, user_id: &UserId, notification_id: Uuid) -> Result<()> {
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND id = $2
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
    pub async fn delete_all_read(&self, user_id: &UserId) -> Result<u64> {
        let result = sqlx::query(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND is_read = TRUE
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
