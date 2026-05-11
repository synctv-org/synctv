use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::models::{ReviewRequestId, ReviewStatus, RoomId, SignupMethod, UserId};
use crate::repository::query_builder::escape_ilike;
use crate::Result;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRegistrationReviewRecord {
    pub id: UserId,
    pub username: String,
    pub email: String,
    pub signup_method: SignupMethod,
    pub status: ReviewStatus,
    pub requested_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<UserId>,
    pub rejection_reason: Option<String>,
    pub oauth2_provider: Option<String>,
    pub oauth2_provider_user_id: Option<String>,
    pub oauth2_provider_username: Option<String>,
    pub oauth2_avatar_url: Option<String>,
    pub oauth2_email_verified: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoomCreationReviewRecord {
    pub id: RoomId,
    pub requested_by: UserId,
    pub requested_by_username: String,
    pub name: String,
    pub description: String,
    pub status: ReviewStatus,
    pub requested_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<UserId>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoomJoinReviewRecord {
    pub id: ReviewRequestId,
    pub room_id: RoomId,
    pub room_name: String,
    pub user_id: UserId,
    pub username: String,
    pub requested_role: i32,
    pub status: ReviewStatus,
    pub requested_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<UserId>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoomJoinReviewListQuery {
    pub status: ReviewStatus,
    pub room_id: Option<RoomId>,
    pub user_id: Option<UserId>,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct RoomCreationReviewListQuery {
    pub status: ReviewStatus,
    pub requested_by: Option<UserId>,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct UserRegistrationReviewListQuery {
    pub status: ReviewStatus,
    pub search: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct ReviewPage<T> {
    pub rows: Vec<T>,
    pub total: i64,
}

#[derive(Clone)]
pub struct ReviewRepository {
    pool: PgPool,
}

impl ReviewRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn load_user_registration(
        &self,
        request_id: UserId,
    ) -> Result<Option<UserRegistrationReviewRecord>> {
        sqlx::query_as::<_, UserRegistrationReviewRecord>(
            r"
            SELECT id, username, COALESCE(email, '') AS email, signup_method, status,
                   requested_at, reviewed_at, reviewed_by, rejection_reason,
                   oauth2_provider, oauth2_provider_user_id, oauth2_provider_username,
                   oauth2_avatar_url, oauth2_email_verified
            FROM user_registration_requests
            WHERE id = $1
            ",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn list_user_registrations(
        &self,
        query: &UserRegistrationReviewListQuery,
    ) -> Result<ReviewPage<UserRegistrationReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .map(escape_ilike)
            .unwrap_or_default();
        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)
            FROM user_registration_requests
            WHERE status = $1
              AND ($2 = '' OR username ILIKE $2 ESCAPE '\' OR COALESCE(email, '') ILIKE $2 ESCAPE '\')
            ",
        )
        .bind(query.status)
        .bind(&search)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, UserRegistrationReviewRecord>(
            r"
            SELECT id, username, COALESCE(email, '') AS email, signup_method, status,
                   requested_at, reviewed_at, reviewed_by, rejection_reason,
                   oauth2_provider, oauth2_provider_user_id, oauth2_provider_username,
                   oauth2_avatar_url, oauth2_email_verified
            FROM user_registration_requests
            WHERE status = $1
              AND ($2 = '' OR username ILIKE $2 ESCAPE '\' OR COALESCE(email, '') ILIKE $2 ESCAPE '\')
            ORDER BY requested_at DESC, id DESC
            LIMIT $3 OFFSET $4
            ",
        )
        .bind(query.status)
        .bind(&search)
        .bind(query.limit)
        .bind(query.offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ReviewPage { rows, total })
    }

    pub async fn reject_user_registration(
        &self,
        request_id: UserId,
        reviewed_by: Option<UserId>,
        reason: &str,
    ) -> Result<u64> {
        let result = sqlx::query(
            r"
            UPDATE user_registration_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3, rejection_reason = $4
            WHERE id = $1 AND reviewed_at IS NULL AND status = $5
            ",
        )
        .bind(request_id)
        .bind(ReviewStatus::Rejected)
        .bind(reviewed_by)
        .bind(reason)
        .bind(ReviewStatus::Pending)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn load_room_creation(
        &self,
        request_id: RoomId,
    ) -> Result<Option<RoomCreationReviewRecord>> {
        sqlx::query_as::<_, RoomCreationReviewRecord>(
            r"
            SELECT rcr.id, rcr.requested_by, COALESCE(u.username, '') AS requested_by_username,
                   rcr.name, rcr.description, rcr.status, rcr.requested_at, rcr.reviewed_at,
                   rcr.reviewed_by, rcr.rejection_reason
            FROM room_creation_requests rcr
            LEFT JOIN users u ON u.id = rcr.requested_by
            WHERE rcr.id = $1
            ",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn list_room_creations(
        &self,
        query: &RoomCreationReviewListQuery,
    ) -> Result<ReviewPage<RoomCreationReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .map(escape_ilike)
            .unwrap_or_default();
        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)
            FROM room_creation_requests
            WHERE status = $1
              AND ($2::bigint IS NULL OR requested_by = $2)
              AND ($3 = '' OR name ILIKE $3 ESCAPE '\' OR description ILIKE $3 ESCAPE '\')
            ",
        )
        .bind(query.status)
        .bind(query.requested_by)
        .bind(&search)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, RoomCreationReviewRecord>(
            r"
            SELECT rcr.id, rcr.requested_by, COALESCE(u.username, '') AS requested_by_username,
                   rcr.name, rcr.description, rcr.status, rcr.requested_at, rcr.reviewed_at,
                   rcr.reviewed_by, rcr.rejection_reason
            FROM room_creation_requests rcr
            LEFT JOIN users u ON u.id = rcr.requested_by
            WHERE rcr.status = $1
              AND ($2::bigint IS NULL OR rcr.requested_by = $2)
              AND ($3 = '' OR rcr.name ILIKE $3 ESCAPE '\' OR rcr.description ILIKE $3 ESCAPE '\')
            ORDER BY rcr.requested_at DESC, rcr.id DESC
            LIMIT $4 OFFSET $5
            ",
        )
        .bind(query.status)
        .bind(query.requested_by)
        .bind(&search)
        .bind(query.limit)
        .bind(query.offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ReviewPage { rows, total })
    }

    pub async fn load_room_join(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        sqlx::query_as::<_, RoomJoinReviewRecord>(&Self::room_join_select_sql("WHERE rjr.id = $1"))
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn load_room_join_in_room(
        &self,
        request_id: ReviewRequestId,
        room_id: RoomId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        sqlx::query_as::<_, RoomJoinReviewRecord>(&Self::room_join_select_sql(
            "WHERE rjr.id = $1 AND rjr.room_id = $2",
        ))
        .bind(request_id)
        .bind(room_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn load_room_join_target(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<(RoomId, UserId)>> {
        let row = sqlx::query_as::<_, (RoomId, UserId)>(
            "SELECT room_id, user_id FROM room_join_requests WHERE id = $1",
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row)
    }

    pub async fn list_room_joins(
        &self,
        query: &RoomJoinReviewListQuery,
    ) -> Result<ReviewPage<RoomJoinReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .map(escape_ilike)
            .unwrap_or_default();
        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*)
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.status = $1
              AND ($2::bigint IS NULL OR rjr.room_id = $2)
              AND ($3::bigint IS NULL OR rjr.user_id = $3)
              AND ($4 = '' OR COALESCE(r.name, '') ILIKE $4 ESCAPE '\' OR COALESCE(u.username, '') ILIKE $4 ESCAPE '\')
            ",
        )
        .bind(query.status)
        .bind(query.room_id)
        .bind(query.user_id)
        .bind(&search)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, RoomJoinReviewRecord>(
            &format!(
                "{} {}",
                Self::room_join_select_sql(
                    r"
                      WHERE rjr.status = $1
                        AND ($2::bigint IS NULL OR rjr.room_id = $2)
                        AND ($3::bigint IS NULL OR rjr.user_id = $3)
                        AND ($4 = '' OR COALESCE(r.name, '') ILIKE $4 ESCAPE '\' OR COALESCE(u.username, '') ILIKE $4 ESCAPE '\')
                    "
                ),
                "ORDER BY rjr.requested_at DESC, rjr.id DESC LIMIT $5 OFFSET $6"
            ),
        )
        .bind(query.status)
        .bind(query.room_id)
        .bind(query.user_id)
        .bind(&search)
        .bind(query.limit)
        .bind(query.offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(ReviewPage { rows, total })
    }

    fn room_join_select_sql(where_clause: &str) -> String {
        format!(
            r"
            SELECT rjr.id, rjr.room_id, COALESCE(r.name, '') AS room_name, rjr.user_id,
                   COALESCE(u.username, '') AS username,
                   COALESCE(rjr.requested_role, 0)::int4 AS requested_role,
                   rjr.status, rjr.requested_at, rjr.reviewed_at, rjr.reviewed_by,
                   rjr.rejection_reason
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            {where_clause}
            "
        )
    }
}
