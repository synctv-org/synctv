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
        let user_id = query.user_id.map(|id| id.as_i64());
        let room_id = query.room_id.map(|id| id.as_i64());
        let total = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "total!" FROM (
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
            "#,
            target_type,
            query.active,
            user_id,
            room_id
        )
        .fetch_one(&self.pool)
        .await?;

        let rows = sqlx::query_as!(
            BanRecordRow,
            r#"
            SELECT id AS "id!: BanRecordId",
                   target_type AS "target_type!",
                   user_id AS "user_id?: UserId",
                   username AS "username!",
                   room_id AS "room_id?: RoomId",
                   room_name AS "room_name!",
                   banned_by AS "banned_by?: UserId",
                   banned_by_username AS "banned_by_username!",
                   reason AS "reason!",
                   starts_at AS "starts_at!",
                   ends_at,
                   revoked_at,
                   revoked_by AS "revoked_by?: UserId",
                   is_active AS "is_active!"
            FROM (
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
            "#,
            target_type,
            query.active,
            user_id,
            room_id,
            query.limit,
            query.offset
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(BanRecordPage { rows, total })
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use synctv_core_testing::create_test_pool;

    use super::*;
    use crate::{
        models::{SignupMethod, User, UserRole, UserStatus},
        repository::UserRepository,
    };

    fn make_user(username: &str, role: UserRole) -> User {
        let now = Utc::now();
        User {
            id: UserId::new(),
            username: username.to_string(),
            role,
            avatar_file_reference_id: None,
            status: UserStatus::Active,
            signup_method: SignupMethod::Email,
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

    #[tokio::test]
    #[ignore = "Requires Docker (testcontainers)"]
    async fn list_returns_active_user_ban_records() {
        let (_postgres, pool) = create_test_pool().await;
        let user_repository = UserRepository::new(pool.clone());
        let admin = user_repository
            .create(&make_user("ban_list_admin", UserRole::Admin))
            .await
            .expect("admin user should be created");
        let target = user_repository
            .create(&make_user("ban_list_target", UserRole::User))
            .await
            .expect("target user should be created");
        let now = Utc::now();
        sqlx::query!(
            r#"
            INSERT INTO user_bans (user_id, banned_by, reason, starts_at)
            VALUES ($1, $2, $3, $4)
            "#,
            target.id.as_i64(),
            admin.id.as_i64(),
            "policy",
            now
        )
        .execute(&pool)
        .await
        .expect("user ban should be inserted");

        let repository = BanRecordRepository::new(pool.clone());
        let page = repository
            .list(&BanRecordListQuery {
                target_type: Some(BanRecordTargetType::User),
                active: Some(true),
                user_id: Some(target.id),
                room_id: None,
                limit: 10,
                offset: 0,
            })
            .await
            .expect("active user ban list should load");

        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
        let row = &page.rows[0];
        assert_eq!(row.target_type, BanRecordTargetType::User.discriminator());
        assert_eq!(row.user_id, Some(target.id));
        assert_eq!(row.username, target.username);
        assert_eq!(row.banned_by, Some(admin.id));
        assert_eq!(row.banned_by_username, admin.username);
        assert_eq!(row.reason, "policy");
        assert!(row.is_active);

        let inactive = repository
            .list(&BanRecordListQuery {
                target_type: Some(BanRecordTargetType::User),
                active: Some(false),
                user_id: Some(target.id),
                room_id: None,
                limit: 10,
                offset: 0,
            })
            .await
            .expect("inactive user ban list should load");
        assert_eq!(inactive.total, 0);
        assert!(inactive.rows.is_empty());
    }
}
