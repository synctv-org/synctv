use crate::{
    models::{
        id::UserId,
        notification::{
            CreateNotificationRequest, Notification, NotificationListQuery, NotificationListSortBy,
            NotificationType,
        },
    },
    Error, Result,
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

fn count_value(value: Option<i64>, query_description: &str) -> Result<i64> {
    value.ok_or_else(|| {
        Error::Internal(format!(
            "{query_description} COUNT query returned no scalar value"
        ))
    })
}

/// Notification repository for database operations
#[derive(Clone, Debug)]
pub struct NotificationRepository {
    pool: PgPool,
}

#[derive(Debug, sqlx::FromRow)]
struct NotificationRow {
    id: i64,
    user_id: UserId,
    notification_type: NotificationType,
    title: String,
    content: String,
    data: serde_json::Value,
    is_read: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<NotificationRow> for Notification {
    fn from(row: NotificationRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            notification_type: row.notification_type,
            title: row.title,
            content: row.content,
            data: row.data,
            is_read: row.is_read,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct NotificationListRow {
    id: i64,
    user_id: UserId,
    notification_type: NotificationType,
    title: String,
    content: String,
    data: serde_json::Value,
    is_read: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl NotificationListRow {
    fn into_notification(self) -> Notification {
        Notification {
            id: self.id,
            user_id: self.user_id,
            notification_type: self.notification_type,
            title: self.title,
            content: self.content,
            data: self.data,
            is_read: self.is_read,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl NotificationRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn push_list_order_by(
        qb: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &NotificationListQuery,
    ) {
        use crate::models::SortDirection;

        let order_by = match (query.sort_by, query.sort_direction) {
            (NotificationListSortBy::Title, SortDirection::Asc) => {
                " ORDER BY title ASC, created_at DESC, id DESC"
            }
            (NotificationListSortBy::Title, SortDirection::Desc) => {
                " ORDER BY title DESC, created_at DESC, id DESC"
            }
            (NotificationListSortBy::UpdatedAt, SortDirection::Asc) => {
                " ORDER BY updated_at ASC, id DESC"
            }
            (NotificationListSortBy::UpdatedAt, SortDirection::Desc) => {
                " ORDER BY updated_at DESC, id DESC"
            }
            (NotificationListSortBy::CreatedAt, SortDirection::Asc) => {
                " ORDER BY created_at ASC, id DESC"
            }
            (NotificationListSortBy::CreatedAt, SortDirection::Desc) => {
                " ORDER BY created_at DESC, id DESC"
            }
        };
        qb.push(order_by);
    }

    fn push_list_filters(
        qb: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        user_id: &UserId,
        query: &NotificationListQuery,
    ) {
        qb.push(" WHERE user_id = ");
        qb.push_bind(user_id);
        qb.push(" AND created_at >= NOW() - INTERVAL '6 months'");

        if let Some(search) = &query.search {
            let pattern = super::query_builder::escape_ilike(search);
            qb.push(" AND (title ILIKE ");
            qb.push_bind(pattern.clone());
            qb.push(" OR content ILIKE ");
            qb.push_bind(pattern);
            qb.push(")");
        }
        if let Some(notification_type) = &query.notification_type {
            qb.push(" AND type = ");
            qb.push_bind(i16::from(*notification_type));
        }
        if let Some(is_read) = query.is_read {
            qb.push(" AND is_read = ");
            qb.push_bind(is_read);
        }
    }

    /// Create a new notification
    pub async fn create(&self, req: &CreateNotificationRequest) -> Result<Notification> {
        let now = Utc::now();

        let row = sqlx::query_as::<_, NotificationRow>(
            r"
            INSERT INTO notifications (user_id, type, title, content, data, is_read, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id,
                      user_id,
                      type AS notification_type,
                      title,
                      content,
                      data,
                      is_read,
                      created_at,
                      updated_at
            ",
        )
        .bind(req.user_id.as_i64())
        .bind(i16::from(req.notification_type))
        .bind(req.title.as_str())
        .bind(req.content.as_str())
        .bind(req.data.clone())
        .bind(false)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.into())
    }

    /// Get notification by ID
    ///
    /// The notifications table is partitioned by `created_at`. Querying by `id`
    /// alone forces `PostgreSQL` to scan every partition. Adding a `created_at`
    /// range filter (last year to now) lets the planner prune irrelevant
    /// partitions and use a targeted index scan instead.
    pub async fn get_by_id(&self, notification_id: i64) -> Result<Option<Notification>> {
        let now = Utc::now();
        let one_year_ago = now - chrono::Duration::days(365);

        let row = sqlx::query_as::<_, NotificationRow>(
            r"
            SELECT id,
                   user_id,
                   type AS notification_type,
                   title,
                   content,
                   data,
                   is_read,
                   created_at,
                   updated_at
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

        Ok(row.map(Into::into))
    }

    /// Get notification by ID, scoped to a user.
    pub async fn get_by_user_and_id(
        &self,
        user_id: &UserId,
        notification_id: i64,
    ) -> Result<Option<Notification>> {
        let now = Utc::now();
        let one_year_ago = now - chrono::Duration::days(365);

        let row = sqlx::query_as::<_, NotificationRow>(
            r"
            SELECT id,
                   user_id,
                   type AS notification_type,
                   title,
                   content,
                   data,
                   is_read,
                   created_at,
                   updated_at
            FROM notifications
            WHERE user_id = $1
              AND id = $2
              AND created_at >= $3
              AND created_at <= $4
            ",
        )
        .bind(user_id.as_i64())
        .bind(notification_id)
        .bind(one_year_ago)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(Into::into))
    }

    /// List notifications for a user with pagination, filters, and total count.
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn list_by_user_with_count(
        &self,
        user_id: &UserId,
        query: &NotificationListQuery,
    ) -> Result<(Vec<Notification>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;

        let mut count_qb: sqlx::QueryBuilder<sqlx::Postgres> =
            sqlx::QueryBuilder::new("SELECT COUNT(*) FROM notifications");
        Self::push_list_filters(&mut count_qb, user_id, query);
        let total = count_qb.build_query_scalar().fetch_one(&self.pool).await?;

        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, user_id, type AS notification_type, title, content, data, is_read, created_at, updated_at \
             FROM notifications",
        );
        Self::push_list_filters(&mut qb, user_id, query);
        Self::push_list_order_by(&mut qb, query);
        qb.push(" LIMIT ");
        qb.push_bind(limit);
        qb.push(" OFFSET ");
        qb.push_bind(offset);

        let rows = qb
            .build_query_as::<NotificationListRow>()
            .fetch_all(&self.pool)
            .await?;

        let notifications = rows
            .into_iter()
            .map(NotificationListRow::into_notification)
            .collect();

        Ok((notifications, total))
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
        qb.push_bind(user_id);

        // Partition pruning: limit scan to the retention window (6 months)
        qb.push(" AND created_at >= NOW() - INTERVAL '6 months'");

        if let Some(notification_type) = notification_type {
            qb.push(" AND type = ");
            qb.push_bind(i16::from(*notification_type));
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
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*)
            FROM notifications
            WHERE user_id = $1 AND is_read = FALSE
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        count_value(count, "unread notification")
    }

    /// Mark notifications as read
    ///
    /// Adds a `created_at >= NOW() - INTERVAL '6 months'` filter to enable
    /// partition pruning on the range-partitioned `notifications` table.
    pub async fn mark_as_read(&self, user_id: &UserId, notification_ids: &[i64]) -> Result<u64> {
        if notification_ids.is_empty() {
            return Ok(0);
        }

        let result = sqlx::query!(
            r"
            UPDATE notifications
            SET is_read = TRUE, updated_at = NOW()
            WHERE user_id = $1 AND id = ANY($2)
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
            user_id as &UserId,
            notification_ids,
        )
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
            sqlx::query!(
                r"
                UPDATE notifications
                SET is_read = TRUE, updated_at = NOW()
                WHERE user_id = $1 AND is_read = FALSE AND created_at <= $2
                  AND created_at >= NOW() - INTERVAL '6 months'
                ",
                user_id as &UserId,
                before_time,
            )
            .execute(&self.pool)
            .await?
        } else {
            sqlx::query!(
                r"
                UPDATE notifications
                SET is_read = TRUE, updated_at = NOW()
                WHERE user_id = $1 AND is_read = FALSE
                  AND created_at >= NOW() - INTERVAL '6 months'
                ",
                user_id as &UserId,
            )
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
    pub async fn delete(&self, user_id: &UserId, notification_id: i64) -> Result<()> {
        let result = sqlx::query!(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND id = $2
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
            user_id as &UserId,
            notification_id,
        )
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
        let result = sqlx::query!(
            r"
            DELETE FROM notifications
            WHERE user_id = $1 AND is_read = TRUE
              AND created_at >= NOW() - INTERVAL '6 months'
            ",
            user_id as &UserId,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Delete all notifications older than the specified number of days,
    /// regardless of read status. Prevents unbounded table growth from
    /// unread notifications that are never acknowledged.
    pub async fn delete_older_than(&self, days: i32) -> Result<u64> {
        let result = sqlx::query!(
            r"
            DELETE FROM notifications
            WHERE created_at < CURRENT_TIMESTAMP - make_interval(days => $1)
            ",
            days,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
