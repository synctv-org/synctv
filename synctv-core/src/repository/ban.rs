use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::models::{BanRecordId, RoomId, UserId};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BanRecordTargetType {
    User,
    Room,
}

impl BanRecordTargetType {
    #[must_use]
    pub const fn discriminator(self) -> i32 {
        match self {
            Self::User => 1,
            Self::Room => 2,
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BanRecordRow {
    pub id: BanRecordId,
    pub target_type: i32,
    pub user_id: Option<UserId>,
    pub username: String,
    pub room_id: Option<RoomId>,
    pub room_name: String,
    pub banned_by: Option<UserId>,
    pub banned_by_username: String,
    pub reason: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<UserId>,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct BanRecordListQuery {
    pub target_type: Option<BanRecordTargetType>,
    pub active: Option<bool>,
    pub user_id: Option<UserId>,
    pub room_id: Option<RoomId>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone)]
pub struct BanRecordPage {
    pub rows: Vec<BanRecordRow>,
    pub total: i64,
}

#[derive(Clone)]
pub struct BanRecordRepository {
    pool: PgPool,
}

impl BanRecordRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list(&self, query: &BanRecordListQuery) -> Result<BanRecordPage> {
        let target_type = query.target_type.map(BanRecordTargetType::discriminator);
        let total = sqlx::query_scalar::<_, i64>(
            r"
            SELECT COUNT(*) FROM (
                SELECT 1::int4 AS target_type, user_id, NULL::bigint AS room_id,
                       revoked_at IS NULL AND (ends_at IS NULL OR ends_at > CURRENT_TIMESTAMP) AS is_active
                FROM user_bans
                UNION ALL
                SELECT 2::int4 AS target_type, NULL::bigint AS user_id, room_id,
                       revoked_at IS NULL AND (ends_at IS NULL OR ends_at > CURRENT_TIMESTAMP) AS is_active
                FROM room_bans
            ) bans
            WHERE ($1::int4 IS NULL OR target_type = $1)
              AND ($2::bool IS NULL OR is_active = $2)
              AND ($3::bigint IS NULL OR user_id = $3)
              AND ($4::bigint IS NULL OR room_id = $4)
            ",
        )
        .bind(target_type)
        .bind(query.active)
        .bind(query.user_id)
        .bind(query.room_id)
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as::<_, BanRecordRow>(
            r"
            SELECT * FROM (
                SELECT ub.id, 1::int4 AS target_type, ub.user_id, COALESCE(u.username, '') AS username,
                       NULL::bigint AS room_id, ''::text AS room_name, ub.banned_by,
                       COALESCE(actor.username, '') AS banned_by_username, COALESCE(ub.reason, '') AS reason,
                       ub.starts_at, ub.ends_at, ub.revoked_at, ub.revoked_by,
                       ub.revoked_at IS NULL AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP) AS is_active
                FROM user_bans ub
                LEFT JOIN users u ON u.id = ub.user_id
                LEFT JOIN users actor ON actor.id = ub.banned_by
                UNION ALL
                SELECT rb.id, 2::int4 AS target_type, NULL::bigint AS user_id, ''::text AS username,
                       rb.room_id, COALESCE(r.name, '') AS room_name, rb.banned_by,
                       COALESCE(actor.username, '') AS banned_by_username, COALESCE(rb.reason, '') AS reason,
                       rb.starts_at, rb.ends_at, rb.revoked_at, rb.revoked_by,
                       rb.revoked_at IS NULL AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP) AS is_active
                FROM room_bans rb
                LEFT JOIN rooms r ON r.id = rb.room_id
                LEFT JOIN users actor ON actor.id = rb.banned_by
            ) bans
            WHERE ($1::int4 IS NULL OR target_type = $1)
              AND ($2::bool IS NULL OR is_active = $2)
              AND ($3::bigint IS NULL OR user_id = $3)
              AND ($4::bigint IS NULL OR room_id = $4)
            ORDER BY starts_at DESC, id DESC
            LIMIT $5 OFFSET $6
            ",
        )
        .bind(target_type)
        .bind(query.active)
        .bind(query.user_id)
        .bind(query.room_id)
        .bind(query.limit)
        .bind(query.offset)
        .fetch_all(&self.pool)
        .await?;

        Ok(BanRecordPage { rows, total })
    }
}
