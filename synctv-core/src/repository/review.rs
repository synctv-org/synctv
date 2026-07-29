use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};

use crate::models::{
    OAuth2Provider, ReviewRequestId, ReviewStatus, RoomCategory, RoomCategoryId, RoomId, RoomLabel,
    RoomLabelId, SignupMethod, UserId,
};
use crate::repository::pools::RepoPools;
use crate::repository::query_builder::ilike_contains_pattern;
use crate::repository::required_count;
use crate::repository::room_taxonomy::{
    optional_room_category_from_parts, OptionalRoomCategoryRowParts,
};
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
    pub oauth2_provider: Option<OAuth2Provider>,
    pub oauth2_provider_instance_name: Option<String>,
    pub oauth2_provider_issuer: Option<String>,
    pub oauth2_provider_user_id: Option<String>,
    pub oauth2_provider_username: Option<String>,
    pub oauth2_avatar_url: Option<String>,
    pub webauthn_credential_id: Option<Vec<u8>>,
    pub webauthn_credential_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoomCreationReviewRecord {
    pub id: RoomId,
    pub requested_by: UserId,
    pub requested_by_username: String,
    pub name: String,
    pub description: String,
    pub category: Option<RoomCategory>,
    pub labels: Vec<RoomLabel>,
    pub status: ReviewStatus,
    pub requested_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<UserId>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RoomCreationReviewRow {
    id: RoomId,
    requested_by: UserId,
    requested_by_username: String,
    name: String,
    description: String,
    category_id: Option<RoomCategoryId>,
    category_key: Option<String>,
    category_name: Option<String>,
    category_description: Option<String>,
    category_sort_order: Option<i32>,
    category_is_enabled: Option<bool>,
    category_created_at: Option<DateTime<Utc>>,
    category_updated_at: Option<DateTime<Utc>>,
    status: ReviewStatus,
    requested_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    reviewed_by: Option<UserId>,
    rejection_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RoomCreationLabelRow {
    request_id: RoomId,
    id: RoomLabelId,
    key: String,
    name: String,
    description: String,
    color: String,
    category_id: Option<RoomCategoryId>,
    sort_order: i32,
    is_enabled: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
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
    pools: RepoPools,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct UserRegistrationReviewRow {
    id: UserId,
    username: String,
    email: String,
    signup_method: SignupMethod,
    status: ReviewStatus,
    requested_at: DateTime<Utc>,
    reviewed_at: Option<DateTime<Utc>>,
    reviewed_by: Option<UserId>,
    rejection_reason: Option<String>,
    oauth2_provider_type: Option<OAuth2Provider>,
    oauth2_provider_instance_name: Option<String>,
    oauth2_provider_issuer: Option<String>,
    oauth2_provider_user_id: Option<String>,
    oauth2_provider_username: Option<String>,
    oauth2_avatar_url: Option<String>,
    webauthn_credential_id: Option<Vec<u8>>,
    webauthn_credential_name: Option<String>,
}

impl TryFrom<UserRegistrationReviewRow> for UserRegistrationReviewRecord {
    type Error = crate::Error;

    fn try_from(row: UserRegistrationReviewRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            username: row.username,
            email: row.email,
            signup_method: row.signup_method,
            status: row.status,
            requested_at: row.requested_at,
            reviewed_at: row.reviewed_at,
            reviewed_by: row.reviewed_by,
            rejection_reason: row.rejection_reason,
            oauth2_provider: row.oauth2_provider_type,
            oauth2_provider_instance_name: row.oauth2_provider_instance_name,
            oauth2_provider_issuer: row.oauth2_provider_issuer,
            oauth2_provider_user_id: row.oauth2_provider_user_id,
            oauth2_provider_username: row.oauth2_provider_username,
            oauth2_avatar_url: row.oauth2_avatar_url,
            webauthn_credential_id: row.webauthn_credential_id,
            webauthn_credential_name: row.webauthn_credential_name,
        })
    }
}

impl ReviewRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
        }
    }

    async fn room_creation_labels_by_request_ids(
        pool: &PgPool,
        request_ids: &[RoomId],
    ) -> Result<std::collections::HashMap<RoomId, Vec<RoomLabel>>> {
        if request_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let ids: Vec<i64> = request_ids.iter().map(RoomId::as_i64).collect();
        let rows = sqlx::query_as!(
            RoomCreationLabelRow,
            r#"
            SELECT rcrl.request_id AS "request_id: RoomId",
                   rl.id AS "id: RoomLabelId",
                   rl.key,
                   rl.name,
                   rl.description,
                   rl.color,
                   rl.category_id AS "category_id: RoomCategoryId",
                   rl.sort_order,
                   rl.is_enabled,
                   rl.created_at,
                   rl.updated_at
            FROM room_creation_request_labels rcrl
            JOIN room_labels rl ON rl.id = rcrl.label_id
            WHERE rcrl.request_id = ANY($1)
            ORDER BY rl.sort_order ASC, rl.id ASC
            "#,
            &ids
        )
        .fetch_all(pool)
        .await?;
        let mut labels_by_request = std::collections::HashMap::new();
        for row in rows {
            labels_by_request
                .entry(row.request_id)
                .or_insert_with(Vec::new)
                .push(RoomLabel {
                    id: row.id,
                    key: row.key,
                    name: row.name,
                    description: row.description,
                    color: row.color,
                    category_id: row.category_id,
                    sort_order: row.sort_order,
                    is_enabled: row.is_enabled,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                });
        }
        Ok(labels_by_request)
    }

    fn room_creation_review_from_row(
        row: RoomCreationReviewRow,
        labels: Vec<RoomLabel>,
    ) -> RoomCreationReviewRecord {
        let category = optional_room_category_from_parts(OptionalRoomCategoryRowParts {
            id: row.category_id,
            key: row.category_key,
            name: row.category_name,
            description: row.category_description,
            sort_order: row.category_sort_order,
            is_enabled: row.category_is_enabled,
            created_at: row.category_created_at,
            updated_at: row.category_updated_at,
        });
        RoomCreationReviewRecord {
            id: row.id,
            requested_by: row.requested_by,
            requested_by_username: row.requested_by_username,
            name: row.name,
            description: row.description,
            category,
            labels,
            status: row.status,
            requested_at: row.requested_at,
            reviewed_at: row.reviewed_at,
            reviewed_by: row.reviewed_by,
            rejection_reason: row.rejection_reason,
        }
    }

    pub async fn load_user_registration(
        &self,
        request_id: UserId,
    ) -> Result<Option<UserRegistrationReviewRecord>> {
        let row = sqlx::query_as!(
            UserRegistrationReviewRow,
            r#"
            SELECT id AS "id: UserId",
                   username,
                   COALESCE(email, '') AS "email!",
                   signup_method AS "signup_method: SignupMethod",
                   status AS "status: ReviewStatus",
                   requested_at,
                   reviewed_at,
                   reviewed_by AS "reviewed_by: UserId",
                   rejection_reason,
                   oauth2_provider_type AS "oauth2_provider_type: OAuth2Provider",
                   oauth2_provider_instance_name,
                   oauth2_provider_issuer,
                   oauth2_provider_user_id,
                   oauth2_provider_username,
                   oauth2_avatar_url,
                   webauthn_credential_id,
                   webauthn_credential_name
            FROM user_registration_requests
            WHERE id = $1
            "#,
            request_id.as_i64()
        )
        .fetch_optional(self.pools.primary())
        .await
        .map_err(crate::Error::from)?;

        row.map(UserRegistrationReviewRecord::try_from).transpose()
    }

    pub async fn list_user_registrations(
        &self,
        query: &UserRegistrationReviewListQuery,
    ) -> Result<ReviewPage<UserRegistrationReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .and_then(ilike_contains_pattern)
            .unwrap_or_default();
        let pool = self.pools.read();
        let total_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)
            FROM user_registration_requests
            WHERE status = $1
              AND ($2 = '' OR username ILIKE $2 ESCAPE '\' OR COALESCE(email, '') ILIKE $2 ESCAPE '\')
            "#,
            i16::from(query.status),
            &search
        )
        .fetch_one(pool)
        .await?;
        let total = required_count(total_count, "user registration review total")?;

        let rows = sqlx::query_as!(
            UserRegistrationReviewRow,
            r#"
            SELECT id AS "id: UserId",
                   username,
                   COALESCE(email, '') AS "email!",
                   signup_method AS "signup_method: SignupMethod",
                   status AS "status: ReviewStatus",
                   requested_at,
                   reviewed_at,
                   reviewed_by AS "reviewed_by: UserId",
                   rejection_reason,
                   oauth2_provider_type AS "oauth2_provider_type: OAuth2Provider",
                   oauth2_provider_instance_name,
                   oauth2_provider_issuer,
                   oauth2_provider_user_id,
                   oauth2_provider_username,
                   oauth2_avatar_url,
                   webauthn_credential_id,
                   webauthn_credential_name
            FROM user_registration_requests
            WHERE status = $1
              AND ($2 = '' OR username ILIKE $2 ESCAPE '\' OR COALESCE(email, '') ILIKE $2 ESCAPE '\')
            ORDER BY requested_at DESC, id DESC
            LIMIT $3 OFFSET $4
            "#,
            i16::from(query.status),
            &search,
            query.limit,
            query.offset
        )
        .fetch_all(pool)
        .await?;

        let rows = rows
            .into_iter()
            .map(UserRegistrationReviewRecord::try_from)
            .collect::<Result<Vec<_>>>()?;

        Ok(ReviewPage { rows, total })
    }

    pub async fn reject_user_registration(
        &self,
        request_id: UserId,
        reviewed_by: Option<UserId>,
        reason: &str,
    ) -> Result<u64> {
        Self::reject_user_registration_with_executor(
            self.pools.primary(),
            request_id,
            reviewed_by,
            reason,
        )
        .await
    }

    pub async fn approve_user_registration_with_executor<'e, E>(
        executor: E,
        request_id: UserId,
        reviewed_by: Option<UserId>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE user_registration_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3
            WHERE id = $1 AND reviewed_at IS NULL AND status = $4
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Approved),
            reviewed_by.map(|id| id.as_i64()),
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn reject_user_registration_with_executor<'e, E>(
        executor: E,
        request_id: UserId,
        reviewed_by: Option<UserId>,
        reason: &str,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE user_registration_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3, rejection_reason = $4
            WHERE id = $1 AND reviewed_at IS NULL AND status = $5
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Rejected),
            reviewed_by.map(|id| id.as_i64()),
            reason,
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn approve_room_creation_with_executor<'e, E>(
        executor: E,
        request_id: RoomId,
        reviewed_by: Option<UserId>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE room_creation_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3
            WHERE id = $1 AND reviewed_at IS NULL AND status = $4
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Approved),
            reviewed_by.map(|id| id.as_i64()),
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn reject_room_creation_with_executor<'e, E>(
        executor: E,
        request_id: RoomId,
        reviewed_by: Option<UserId>,
        reason: Option<&str>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE room_creation_requests
            SET status = $2, reviewed_at = CURRENT_TIMESTAMP, reviewed_by = $3, rejection_reason = $4
            WHERE id = $1 AND reviewed_at IS NULL AND status = $5
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Rejected),
            reviewed_by.map(|id| id.as_i64()),
            reason,
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn load_room_creation(
        &self,
        request_id: RoomId,
    ) -> Result<Option<RoomCreationReviewRecord>> {
        let row = sqlx::query_as!(
            RoomCreationReviewRow,
            r#"
            SELECT rcr.id AS "id!: RoomId",
                   rcr.requested_by AS "requested_by!: UserId",
                   COALESCE(u.username, '') AS "requested_by_username!",
                   rcr.name AS "name!",
                   rcr.description AS "description!",
                   rc.id AS "category_id: RoomCategoryId",
                   rc.key AS "category_key?",
                   rc.name AS "category_name?",
                   rc.description AS "category_description?",
                   rc.sort_order AS "category_sort_order?",
                   rc.is_enabled AS "category_is_enabled?",
                   rc.created_at AS "category_created_at?",
                   rc.updated_at AS "category_updated_at?",
                   rcr.status AS "status!: ReviewStatus",
                   rcr.requested_at AS "requested_at!",
                   rcr.reviewed_at AS "reviewed_at?",
                   rcr.reviewed_by AS "reviewed_by?: UserId",
                   rcr.rejection_reason AS "rejection_reason?"
            FROM room_creation_requests rcr
            LEFT JOIN users u ON u.id = rcr.requested_by
            LEFT JOIN room_categories rc ON rc.id = rcr.category_id
            WHERE rcr.id = $1
            "#,
            request_id.as_i64()
        )
        .fetch_optional(self.pools.primary())
        .await
        .map_err(crate::Error::from)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let labels = Self::room_creation_labels_by_request_ids(self.pools.primary(), &[request_id])
            .await?
            .remove(&request_id)
            .unwrap_or_default();
        Ok(Some(Self::room_creation_review_from_row(row, labels)))
    }

    pub async fn list_room_creations(
        &self,
        query: &RoomCreationReviewListQuery,
    ) -> Result<ReviewPage<RoomCreationReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .and_then(ilike_contains_pattern)
            .unwrap_or_default();
        let pool = self.pools.read();
        let total_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)
            FROM room_creation_requests
            WHERE status = $1
              AND ($2::bigint IS NULL OR requested_by = $2)
              AND ($3 = '' OR name ILIKE $3 ESCAPE '\' OR description ILIKE $3 ESCAPE '\')
            "#,
            i16::from(query.status),
            query.requested_by.map(|id| id.as_i64()),
            &search
        )
        .fetch_one(pool)
        .await?;
        let total = required_count(total_count, "room creation review total")?;

        let rows = sqlx::query_as!(
            RoomCreationReviewRow,
            r#"
            SELECT rcr.id AS "id!: RoomId",
                   rcr.requested_by AS "requested_by!: UserId",
                   COALESCE(u.username, '') AS "requested_by_username!",
                   rcr.name AS "name!",
                   rcr.description AS "description!",
                   rc.id AS "category_id: RoomCategoryId",
                   rc.key AS "category_key?",
                   rc.name AS "category_name?",
                   rc.description AS "category_description?",
                   rc.sort_order AS "category_sort_order?",
                   rc.is_enabled AS "category_is_enabled?",
                   rc.created_at AS "category_created_at?",
                   rc.updated_at AS "category_updated_at?",
                   rcr.status AS "status!: ReviewStatus",
                   rcr.requested_at AS "requested_at!",
                   rcr.reviewed_at AS "reviewed_at?",
                   rcr.reviewed_by AS "reviewed_by?: UserId",
                   rcr.rejection_reason AS "rejection_reason?"
            FROM room_creation_requests rcr
            LEFT JOIN users u ON u.id = rcr.requested_by
            LEFT JOIN room_categories rc ON rc.id = rcr.category_id
            WHERE rcr.status = $1
              AND ($2::bigint IS NULL OR rcr.requested_by = $2)
              AND ($3 = '' OR rcr.name ILIKE $3 ESCAPE '\' OR rcr.description ILIKE $3 ESCAPE '\')
            ORDER BY rcr.requested_at DESC, rcr.id DESC
            LIMIT $4 OFFSET $5
            "#,
            i16::from(query.status),
            query.requested_by.map(|id| id.as_i64()),
            &search,
            query.limit,
            query.offset
        )
        .fetch_all(pool)
        .await?;

        let request_ids: Vec<RoomId> = rows.iter().map(|row| row.id).collect();
        let mut labels_by_request =
            Self::room_creation_labels_by_request_ids(pool, &request_ids).await?;
        let rows = rows
            .into_iter()
            .map(|row| {
                let labels = labels_by_request.remove(&row.id).unwrap_or_default();
                Self::room_creation_review_from_row(row, labels)
            })
            .collect();

        Ok(ReviewPage { rows, total })
    }

    pub async fn load_room_join(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        sqlx::query_as!(
            RoomJoinReviewRecord,
            r#"
            SELECT rjr.id AS "id!: ReviewRequestId",
                   rjr.room_id AS "room_id!: RoomId",
                   COALESCE(r.name, '') AS "room_name!",
                   rjr.user_id AS "user_id!: UserId",
                   COALESCE(u.username, '') AS "username!",
                   COALESCE(rjr.requested_role, 0)::int4 AS "requested_role!",
                   rjr.status AS "status!: ReviewStatus",
                   rjr.requested_at AS "requested_at!",
                   rjr.reviewed_at AS "reviewed_at?",
                   rjr.reviewed_by AS "reviewed_by?: UserId",
                   rjr.rejection_reason AS "rejection_reason?"
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.id = $1
            "#,
            request_id.as_i64()
        )
        .fetch_optional(self.pools.primary())
        .await
        .map_err(Into::into)
    }

    pub async fn load_room_join_in_room(
        &self,
        request_id: ReviewRequestId,
        room_id: RoomId,
    ) -> Result<Option<RoomJoinReviewRecord>> {
        sqlx::query_as!(
            RoomJoinReviewRecord,
            r#"
            SELECT rjr.id AS "id!: ReviewRequestId",
                   rjr.room_id AS "room_id!: RoomId",
                   COALESCE(r.name, '') AS "room_name!",
                   rjr.user_id AS "user_id!: UserId",
                   COALESCE(u.username, '') AS "username!",
                   COALESCE(rjr.requested_role, 0)::int4 AS "requested_role!",
                   rjr.status AS "status!: ReviewStatus",
                   rjr.requested_at AS "requested_at!",
                   rjr.reviewed_at AS "reviewed_at?",
                   rjr.reviewed_by AS "reviewed_by?: UserId",
                   rjr.rejection_reason AS "rejection_reason?"
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.id = $1 AND rjr.room_id = $2
            "#,
            request_id.as_i64(),
            room_id.as_i64()
        )
        .fetch_optional(self.pools.primary())
        .await
        .map_err(Into::into)
    }

    pub async fn load_room_join_target(
        &self,
        request_id: ReviewRequestId,
    ) -> Result<Option<(RoomId, UserId)>> {
        let row = sqlx::query!(
            r#"
            SELECT room_id AS "room_id: RoomId",
                   user_id AS "user_id: UserId"
            FROM room_join_requests
            WHERE id = $1
            "#,
            request_id.as_i64()
        )
        .fetch_optional(self.pools.primary())
        .await?;

        Ok(row.map(|row| (row.room_id, row.user_id)))
    }

    pub async fn approve_room_join_by_member_with_executor<'e, E>(
        executor: E,
        room_id: RoomId,
        user_id: UserId,
        reviewed_by: Option<UserId>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE room_join_requests
            SET status = $3,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $4
            WHERE room_id = $1
              AND user_id = $2
              AND reviewed_at IS NULL
              AND status = $5
            ",
            room_id.as_i64(),
            user_id.as_i64(),
            i16::from(ReviewStatus::Approved),
            reviewed_by.map(|id| id.as_i64()),
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn approve_room_join_with_executor<'e, E>(
        executor: E,
        request_id: ReviewRequestId,
        room_id: RoomId,
        reviewed_by: Option<UserId>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE room_join_requests
            SET status = $2,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $3
            WHERE id = $1
              AND room_id = $4
              AND reviewed_at IS NULL
              AND status = $5
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Approved),
            reviewed_by.map(|id| id.as_i64()),
            room_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn reject_room_join_with_executor<'e, E>(
        executor: E,
        request_id: ReviewRequestId,
        room_id: RoomId,
        reviewed_by: Option<UserId>,
        reason: Option<&str>,
    ) -> Result<u64>
    where
        E: PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE room_join_requests
            SET status = $2,
                reviewed_at = CURRENT_TIMESTAMP,
                reviewed_by = $3,
                rejection_reason = $4
            WHERE id = $1
              AND room_id = $5
              AND reviewed_at IS NULL
              AND status = $6
            ",
            request_id.as_i64(),
            i16::from(ReviewStatus::Rejected),
            reviewed_by.map(|id| id.as_i64()),
            reason,
            room_id.as_i64(),
            i16::from(ReviewStatus::Pending)
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    pub async fn list_room_joins(
        &self,
        query: &RoomJoinReviewListQuery,
    ) -> Result<ReviewPage<RoomJoinReviewRecord>> {
        let search = query
            .search
            .as_deref()
            .and_then(ilike_contains_pattern)
            .unwrap_or_default();
        let pool = self.pools.read();
        let total_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.status = $1
              AND ($2::bigint IS NULL OR rjr.room_id = $2)
              AND ($3::bigint IS NULL OR rjr.user_id = $3)
              AND ($4 = '' OR COALESCE(r.name, '') ILIKE $4 ESCAPE '\' OR COALESCE(u.username, '') ILIKE $4 ESCAPE '\')
            "#,
            i16::from(query.status),
            query.room_id.map(|id| id.as_i64()),
            query.user_id.map(|id| id.as_i64()),
            &search
        )
        .fetch_one(pool)
        .await?;
        let total = required_count(total_count, "room join review total")?;

        let rows = sqlx::query_as!(
            RoomJoinReviewRecord,
            r#"
            SELECT rjr.id AS "id: ReviewRequestId",
                   rjr.room_id AS "room_id: RoomId",
                   COALESCE(r.name, '') AS "room_name!",
                   rjr.user_id AS "user_id: UserId",
                   COALESCE(u.username, '') AS "username!",
                   COALESCE(rjr.requested_role, 0)::int4 AS "requested_role!",
                   rjr.status AS "status: ReviewStatus",
                   rjr.requested_at,
                   rjr.reviewed_at,
                   rjr.reviewed_by AS "reviewed_by: UserId",
                   rjr.rejection_reason
            FROM room_join_requests rjr
            LEFT JOIN rooms r ON r.id = rjr.room_id
            LEFT JOIN users u ON u.id = rjr.user_id
            WHERE rjr.status = $1
              AND ($2::bigint IS NULL OR rjr.room_id = $2)
              AND ($3::bigint IS NULL OR rjr.user_id = $3)
              AND ($4 = '' OR COALESCE(r.name, '') ILIKE $4 ESCAPE '\' OR COALESCE(u.username, '') ILIKE $4 ESCAPE '\')
            ORDER BY rjr.requested_at DESC, rjr.id DESC
            LIMIT $5 OFFSET $6
            "#,
            i16::from(query.status),
            query.room_id.map(|id| id.as_i64()),
            query.user_id.map(|id| id.as_i64()),
            &search,
            query.limit,
            query.offset
        )
        .fetch_all(pool)
        .await?;

        Ok(ReviewPage { rows, total })
    }
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
