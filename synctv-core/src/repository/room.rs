use sqlx::{FromRow, PgConnection, PgPool, Row};

use super::query_builder::{escape_ilike, WhereClauseBuilder};
use crate::{
    models::{
        PageParams, Room, RoomId, RoomListQuery, RoomListSortBy, RoomSettings, RoomStatus, UserId,
    },
    Result,
};

#[derive(Debug, sqlx::FromRow)]
struct RoomRow {
    id: RoomId,
    name: String,
    description: String,
    created_by: UserId,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    is_banned: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    version: i32,
    last_activity_at: chrono::DateTime<chrono::Utc>,
}

impl From<RoomRow> for Room {
    fn from(row: RoomRow) -> Self {
        let status = if row.closed_at.is_some() {
            RoomStatus::Closed
        } else {
            RoomStatus::Active
        };
        Self {
            id: row.id,
            name: row.name,
            description: row.description,
            created_by: row.created_by,
            status,
            is_banned: row.is_banned,
            closed_at: row.closed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
            version: row.version,
            last_activity_at: row.last_activity_at,
        }
    }
}

const ROOM_SELECT_COLUMNS: &str = "r.id, r.name, r.description, r.created_by, r.closed_at,
    r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
    EXISTS (
        SELECT 1 FROM room_bans rb
        WHERE rb.room_id = r.id
          AND rb.revoked_at IS NULL
          AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
    ) AS is_banned";
const ACTIVE_ROOM_BAN_EXISTS_SQL: &str = "EXISTS (
    SELECT 1 FROM room_bans rb
    WHERE rb.room_id = r.id
      AND rb.revoked_at IS NULL
      AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
)";
const ACTIVE_ROOM_BAN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM room_bans rb
    WHERE rb.room_id = r.id
      AND rb.revoked_at IS NULL
      AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
)";
const ACTIVE_ROOM_MEMBER_BAN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM room_member_bans rmb
    WHERE rmb.room_id = rm.room_id
      AND rmb.user_id = rm.user_id
      AND rmb.revoked_at IS NULL
      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
)";

/// Pre-fetched context for the join-room flow, retrieved in a single DB round-trip.
#[derive(Debug)]
pub struct JoinRoomContext {
    pub room: Room,
    pub is_banned: bool,
    pub settings: RoomSettings,
    pub password_hash: Option<String>,
}

/// Room repository for database operations
#[derive(Clone)]
pub struct RoomRepository {
    pool: PgPool,
}

const ACCESSIBLE_ROOM_CREATOR_CONDITION: &str =
    "EXISTS (SELECT 1 FROM users u WHERE u.id = r.created_by AND u.deleted_at IS NULL
        AND NOT EXISTS (
            SELECT 1 FROM user_bans ub
            WHERE ub.user_id = u.id
              AND ub.revoked_at IS NULL
              AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ))";

fn pagination_u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn count_i64_to_i32(count: i64) -> Result<i32> {
    i32::try_from(count).map_err(|_| {
        crate::Error::Internal(format!(
            "Count {count} exceeds i32::MAX; pagination contract violated"
        ))
    })
}

impl RoomRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new room
    ///
    /// Relies on the database UNIQUE constraint on `(created_by, name)` to
    /// reject duplicate active room names for the same creator atomically (no
    /// TOCTOU race condition).
    pub async fn create(&self, room: &Room) -> Result<Room> {
        self.create_with_executor(room, &self.pool).await
    }

    /// Create a new room using a provided executor (pool or transaction)
    ///
    /// Relies on the database UNIQUE constraint on `(created_by, name)` to
    /// reject duplicate active room names for the same creator atomically (no
    /// TOCTOU race condition).
    pub async fn create_with_executor<'e, E>(&self, room: &Room, executor: E) -> Result<Room>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let created = sqlx::query_as!(
            RoomRow,
            r#"
             INSERT INTO rooms (name, description, created_by, closed_at, created_at, updated_at, version, last_activity_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id AS "id: RoomId",
                       name,
                       description,
                       created_by AS "created_by: UserId",
                       closed_at,
                       false AS "is_banned!",
                       created_at,
                       updated_at,
                       deleted_at,
                       version,
                       last_activity_at
            "#,
            &room.name,
            &room.description,
            room.created_by.as_i64(),
            room.closed_at,
            room.created_at,
            room.updated_at,
            room.version,
            room.last_activity_at,
        )
            .fetch_one(executor)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                    let constraint = db_err.constraint().unwrap_or("");
                    if constraint.contains("idx_rooms_created_by_name")
                        || constraint.contains("rooms_created_by_name")
                    {
                        crate::Error::AlreadyExists(
                            "You already have a room with this name".to_string(),
                        )
                    } else {
                        crate::Error::Database(e)
                    }
                }
                _ => crate::Error::Database(e),
            })?;

        Ok(created.into())
    }

    /// Get room by ID
    pub async fn get_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.created_by AS "created_by: UserId",
                   r.closed_at,
                   EXISTS (
                       SELECT 1 FROM room_bans rb
                       WHERE rb.room_id = r.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS "is_banned!",
                   r.created_at,
                   r.updated_at,
                   r.deleted_at,
                   r.version,
                   r.last_activity_at
            FROM rooms r
            WHERE r.id = $1 AND r.deleted_at IS NULL
            "#,
            room_id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(room.map(Into::into))
    }

    /// Update room with optimistic locking.
    ///
    /// The caller must pass the `version` value from the previously-read room.
    /// The update atomically increments `version` in the database and only
    /// succeeds when the row's `version` still matches `old_version`.
    ///
    /// Using an integer version column avoids two problems with timestamp-based
    /// locking:
    /// - Clock skew between the DB server and app server causing spurious conflicts.
    /// - Two updates in the same millisecond both seeing the same timestamp.
    ///
    /// Returns `Error::OptimisticLockConflict` when another concurrent update
    /// already changed the row, so the caller can retry with a fresh read.
    ///
    /// Note: `updated_at` is set automatically by the `update_rooms_updated_at`
    /// BEFORE UPDATE trigger, so we omit it from the SET clause.
    pub async fn update(&self, room: &Room, old_version: i32) -> Result<Room> {
        let updated = sqlx::query_as!(
            RoomRow,
            r#"
             UPDATE rooms
             SET name = $2, description = $3, closed_at = $4, version = version + 1
             WHERE id = $1 AND deleted_at IS NULL AND version = $5
             RETURNING id AS "id: RoomId",
                       name,
                       description,
                       created_by AS "created_by: UserId",
                       closed_at,
                       false AS "is_banned!",
                       created_at,
                       updated_at,
                       deleted_at,
                       version,
                       last_activity_at
            "#,
            room.id.as_i64(),
            &room.name,
            &room.description,
            room.closed_at,
            old_version,
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(updated) = updated {
            Ok(updated.into())
        } else {
            // Check if the room exists at all to distinguish
            // "not found" from "concurrent modification"
            let exists = self.get_by_id(&room.id).await?.is_some();
            if exists {
                Err(crate::Error::OptimisticLockConflict)
            } else {
                Err(crate::Error::NotFound(format!(
                    "Room {} not found",
                    room.id
                )))
            }
        }
    }

    /// Soft delete room
    pub async fn delete(&self, room_id: &RoomId) -> Result<bool> {
        let result = sqlx::query!(
            "UPDATE rooms
             SET deleted_at = $2, version = version + 1
             WHERE id = $1 AND deleted_at IS NULL",
            room_id as &RoomId,
            chrono::Utc::now(),
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Hard delete room (used for cleanup of partially created rooms).
    pub async fn hard_delete(&self, room_id: &RoomId) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let deleted =
            crate::service::room::hard_delete_room_and_cleanup_in_tx(&mut tx, room_id).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    /// Build the shared WHERE clause conditions for room list queries.
    fn build_room_list_conditions(query: &RoomListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("r.deleted_at IS NULL");

        match &query.status {
            Some(RoomStatus::Active) => wb.push_literal("r.closed_at IS NULL"),
            Some(RoomStatus::Closed) => wb.push_literal("r.closed_at IS NOT NULL"),
            None => {}
        }

        match query.is_banned {
            Some(true) => wb.push_literal(ACTIVE_ROOM_BAN_EXISTS_SQL),
            Some(false) => wb.push_literal(ACTIVE_ROOM_BAN_NOT_EXISTS_SQL),
            None => {}
        }

        if query.search.is_some() {
            wb.push_param("(r.name ILIKE ${idx} OR r.description ILIKE ${idx})");
        }

        if query.creator_id.is_some() {
            wb.push_param("r.created_by = ${idx}");
        }

        wb
    }

    /// Bind the filter parameters (search, creator_id) onto a `query_scalar` in
    /// the same order they appear in `build_room_list_conditions`.
    fn bind_filters_scalar<'q>(
        qb: sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments>,
        query: &'q RoomListQuery,
        search_pattern: Option<&'q String>,
    ) -> sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments> {
        let qb = match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        };
        let qb = match &query.creator_id {
            Some(creator_id) => qb.bind(creator_id),
            None => qb,
        };
        qb
    }

    /// Bind the filter parameters (search, creator_id) onto a `query_as` in
    /// the same order they appear in `build_room_list_conditions`.
    fn bind_filters<'q, O>(
        qb: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
        query: &'q RoomListQuery,
        search_pattern: Option<&'q String>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        let qb = match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        };
        let qb = match &query.creator_id {
            Some(creator_id) => qb.bind(creator_id),
            None => qb,
        };
        qb
    }

    fn build_order_by(query: &RoomListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            RoomListSortBy::Name => format!("r.name {direction}, r.id {direction}"),
            RoomListSortBy::UpdatedAt => format!("r.updated_at {direction}, r.id {direction}"),
            RoomListSortBy::LastActivityAt => {
                format!("r.last_activity_at {direction} NULLS LAST, r.id {direction}")
            }
            RoomListSortBy::CreatedAt => format!("r.created_at {direction}, r.id {direction}"),
        }
    }

    /// List rooms with pagination and filters
    pub async fn list(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let wb = Self::build_room_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!("SELECT COUNT(*) as count FROM rooms r WHERE {count_where}");
        let count: i64 = Self::bind_filters_scalar(
            sqlx::query_scalar(&count_sql),
            query,
            search_pattern.as_ref(),
        )
        .fetch_one(&self.pool)
        .await?;

        // List query: $1=limit, $2=offset, then filter params start at $3
        let (list_where, _) = wb.build(3);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            "SELECT {ROOM_SELECT_COLUMNS}
             FROM rooms r
             WHERE {list_where}
             ORDER BY {order_by}
             LIMIT $1 OFFSET $2"
        );
        let list_qb = sqlx::query_as::<_, Room>(&list_sql)
            .bind(limit)
            .bind(offset);
        let rooms: Vec<Room> = Self::bind_filters(list_qb, query, search_pattern.as_ref())
            .fetch_all(&self.pool)
            .await?;

        Ok((rooms, count))
    }

    /// List only rooms whose creator is still active.
    pub async fn list_accessible(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let wb = Self::build_room_list_conditions(query);

        let (count_where, _) = wb.build(1);
        let count_sql = format!(
            "SELECT COUNT(*) as count FROM rooms r WHERE {count_where} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}"
        );
        let count: i64 = Self::bind_filters_scalar(
            sqlx::query_scalar(&count_sql),
            query,
            search_pattern.as_ref(),
        )
        .fetch_one(&self.pool)
        .await?;

        let (list_where, _) = wb.build(3);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            "SELECT {ROOM_SELECT_COLUMNS}
             FROM rooms r
             WHERE {list_where} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}
             ORDER BY {order_by}
             LIMIT $1 OFFSET $2"
        );
        let list_qb = sqlx::query_as::<_, Room>(&list_sql)
            .bind(limit)
            .bind(offset);
        let rooms: Vec<Room> = Self::bind_filters(list_qb, query, search_pattern.as_ref())
            .fetch_all(&self.pool)
            .await?;

        Ok((rooms, count))
    }

    /// List rooms related to a user, either by ownership or active membership.
    pub async fn list_related_to_user(
        &self,
        user_id: &UserId,
        query: &RoomListQuery,
    ) -> Result<(Vec<Room>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_room_list_conditions(query);
        let relation_sql = "(r.created_by = $1 OR EXISTS (
                SELECT 1
                FROM room_members rm
                WHERE rm.room_id = r.id
                  AND rm.user_id = $1
                  AND rm.left_at IS NULL
            ))";

        let (count_where, _) = wb.build(2);
        let count_sql = format!(
            "SELECT COUNT(*) as count
             FROM rooms r
             WHERE {relation_sql} AND {count_where}"
        );
        let count: i64 = Self::bind_filters_scalar(
            sqlx::query_scalar(&count_sql).bind(user_id),
            query,
            search_pattern.as_ref(),
        )
        .fetch_one(&self.pool)
        .await?;

        let (list_where, _) = wb.build(4);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            "SELECT {ROOM_SELECT_COLUMNS}
             FROM rooms r
             WHERE {relation_sql} AND {list_where}
             ORDER BY {order_by}
             LIMIT $2 OFFSET $3"
        );
        let rooms: Vec<Room> = Self::bind_filters(
            sqlx::query_as::<_, Room>(&list_sql)
                .bind(user_id)
                .bind(limit)
                .bind(offset),
            query,
            search_pattern.as_ref(),
        )
        .fetch_all(&self.pool)
        .await?;

        Ok((rooms, count))
    }

    /// List rooms with member count (optimized with JOIN)
    pub async fn list_with_count(
        &self,
        query: &RoomListQuery,
    ) -> Result<(Vec<crate::models::RoomWithCount>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let wb = Self::build_room_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!("SELECT COUNT(DISTINCT r.id) FROM rooms r WHERE {count_where}");
        let count: i64 = Self::bind_filters_scalar(
            sqlx::query_scalar(&count_sql),
            query,
            search_pattern.as_ref(),
        )
        .fetch_one(&self.pool)
        .await?;

        // List query: $1=limit, $2=offset, then filter params start at $3
        let (list_where, _) = wb.build(3);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            r"
            SELECT
                {ROOM_SELECT_COLUMNS},
                COALESCE(COUNT(rm.user_id) FILTER (
                    WHERE rm.left_at IS NULL AND {ACTIVE_ROOM_MEMBER_BAN_NOT_EXISTS_SQL}
                ), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE {list_where}
            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at
            ORDER BY {order_by}
            LIMIT $1 OFFSET $2
            "
        );

        let mut list_qb = sqlx::query(&list_sql).bind(limit).bind(offset);
        if let Some(ref pattern) = search_pattern {
            list_qb = list_qb.bind(pattern);
        }
        if let Some(ref creator_id) = query.creator_id {
            list_qb = list_qb.bind(creator_id);
        }
        let rows = list_qb.fetch_all(&self.pool).await?;

        let rooms_with_count: Result<Vec<crate::models::RoomWithCount>> = rows
            .into_iter()
            .map(|row| {
                let member_count: i32 = row.try_get("member_count")?;
                let room = Room::from_row(&row)?;
                Ok(crate::models::RoomWithCount { room, member_count })
            })
            .collect();

        Ok((rooms_with_count?, count))
    }

    /// Check if room exists (not soft-deleted)
    pub async fn exists(&self, room_id: &RoomId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM rooms
                WHERE id = $1 AND deleted_at IS NULL
            ) as "exists!"
            "#,
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Check if room exists, is active, and is not banned
    ///
    /// This is a stricter check than `exists()` -- it also verifies the room
    /// has status = Active and is not banned, which is the condition for a room
    /// to be joinable/accessible by regular users.
    pub async fn is_accessible(&self, room_id: &RoomId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM rooms
                WHERE id = $1
                  AND deleted_at IS NULL
                  AND closed_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM room_bans rb
                      WHERE rb.room_id = rooms.id
                        AND rb.revoked_at IS NULL
                        AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                  )
            ) as "exists!"
            "#,
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Get room member count (excludes banned members)
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) as count
            FROM room_members rm
            WHERE rm.room_id = $1
              AND rm.left_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_bans rmb
                  WHERE rmb.room_id = rm.room_id
                    AND rmb.user_id = rm.user_id
                    AND rmb.revoked_at IS NULL
                    AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
              )
            ",
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        count_i64_to_i32(count)
    }

    /// Get rooms created by a specific user
    pub async fn list_by_creator(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        let limit = pagination_u64_to_i64(pagination.limit());
        let offset = pagination_u64_to_i64(pagination.offset());

        // Get total count
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL",
            creator_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        // Get rooms
        let sql = format!(
            "SELECT {ROOM_SELECT_COLUMNS}
             FROM rooms r
             WHERE r.created_by = $1 AND r.deleted_at IS NULL
             ORDER BY created_at DESC
             LIMIT $2 OFFSET $3"
        );
        let rooms = sqlx::query_as::<_, Room>(&sql)
            .bind(creator_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok((rooms, count))
    }

    /// Get rooms created by a specific user with member count (optimized)
    pub async fn list_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<crate::models::RoomWithCount>, i64)> {
        let limit = pagination_u64_to_i64(pagination.limit());
        let offset = pagination_u64_to_i64(pagination.offset());

        // Get total count
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL",
            creator_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?
        .unwrap_or(0);

        // Get rooms with member count using LEFT JOIN
        let sql = format!(
            r"
            SELECT
                {ROOM_SELECT_COLUMNS},
                COALESCE(COUNT(rm.user_id) FILTER (
                    WHERE rm.left_at IS NULL AND {ACTIVE_ROOM_MEMBER_BAN_NOT_EXISTS_SQL}
                ), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE r.created_by = $1 AND r.deleted_at IS NULL
            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at
            ORDER BY r.created_at DESC
            LIMIT $2 OFFSET $3
            "
        );
        let rows = sqlx::query(&sql)
            .bind(creator_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        let rooms_with_count: Result<Vec<crate::models::RoomWithCount>> = rows
            .into_iter()
            .map(|row| {
                let member_count: i32 = row.try_get("member_count")?;
                let room = Room::from_row(&row)?;
                Ok(crate::models::RoomWithCount { room, member_count })
            })
            .collect();

        Ok((rooms_with_count?, count))
    }

    /// Update derived room status by closing or reopening the room.
    ///
    /// This intentionally does NOT use optimistic locking (CAS) because status
    /// updates are idempotent flag-sets: setting a room to `Active` twice produces
    /// the same result. The `version` column is still incremented to propagate
    /// cache invalidation, but no `WHERE version = ?` guard is needed.
    pub async fn update_status(&self, room_id: &RoomId, status: RoomStatus) -> Result<Room> {
        let closed_at = match status {
            RoomStatus::Active => None,
            RoomStatus::Closed => Some(chrono::Utc::now()),
        };
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            UPDATE rooms
            SET closed_at = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id AS "id: RoomId",
                      name,
                      description,
                      created_by AS "created_by: UserId",
                      closed_at,
                      false AS "is_banned!",
                      created_at,
                      updated_at,
                      deleted_at,
                      version,
                      last_activity_at
            "#,
            closed_at,
            room_id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;
        Ok(room.into())
    }

    /// Update room ban policy using `room_bans`.
    pub async fn update_ban_status(&self, room_id: &RoomId, is_banned: bool) -> Result<Room> {
        if is_banned {
            sqlx::query!(
                r"
                INSERT INTO room_bans (room_id, starts_at)
                SELECT r.id, CURRENT_TIMESTAMP
                FROM rooms r
                WHERE r.id = $1 AND r.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM room_bans rb
                      WHERE rb.room_id = r.id
                        AND rb.revoked_at IS NULL
                        AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                      )
                ",
                room_id as &RoomId,
            )
            .execute(&self.pool)
            .await?;
        } else {
            sqlx::query!(
                r"
                UPDATE room_bans rb
                SET revoked_at = CURRENT_TIMESTAMP
                FROM rooms r
                WHERE rb.room_id = r.id
                  AND r.id = $1
                  AND r.deleted_at IS NULL
                  AND rb.revoked_at IS NULL
                  AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                ",
                room_id as &RoomId,
            )
            .execute(&self.pool)
            .await?;
        }

        let room = self
            .get_by_id(room_id)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(room)
    }

    pub async fn update_ban_status_with_executor(
        room_id: &RoomId,
        is_banned: bool,
        executor: &mut PgConnection,
    ) -> Result<Room> {
        if is_banned {
            sqlx::query!(
                r"
                INSERT INTO room_bans (room_id, starts_at)
                SELECT r.id, CURRENT_TIMESTAMP
                FROM rooms r
                WHERE r.id = $1 AND r.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM room_bans rb
                      WHERE rb.room_id = r.id
                        AND rb.revoked_at IS NULL
                        AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                      )
                ",
                room_id as &RoomId,
            )
            .execute(&mut *executor)
            .await?;
        } else {
            sqlx::query!(
                r"
                UPDATE room_bans rb
                SET revoked_at = CURRENT_TIMESTAMP
                FROM rooms r
                WHERE rb.room_id = r.id
                  AND r.id = $1
                  AND r.deleted_at IS NULL
                  AND rb.revoked_at IS NULL
                  AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                ",
                room_id as &RoomId,
            )
            .execute(&mut *executor)
            .await?;
        }

        let room = sqlx::query_as::<_, RoomRow>(
            r"
            SELECT r.id,
                   r.name,
                   r.description,
                   r.created_by,
                   r.closed_at,
                   r.created_at,
                   r.updated_at,
                   r.deleted_at,
                   r.version,
                   r.last_activity_at,
                   EXISTS (
                       SELECT 1 FROM room_bans rb
                       WHERE rb.room_id = r.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS is_banned
            FROM rooms r
            WHERE r.id = $1 AND r.deleted_at IS NULL
            ",
        )
        .bind(room_id.as_i64())
        .fetch_optional(&mut *executor)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(room.into())
    }

    /// Update room description
    pub async fn update_description(&self, room_id: &RoomId, description: &str) -> Result<Room> {
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            UPDATE rooms
            SET description = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id AS "id: RoomId",
                      name,
                      description,
                      created_by AS "created_by: UserId",
                      closed_at,
                      false AS "is_banned!",
                      created_at,
                      updated_at,
                      deleted_at,
                      version,
                      last_activity_at
            "#,
            description,
            room_id.as_i64(),
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(room.into())
    }

    /// Transfer room ownership inside an existing transaction.
    pub async fn transfer_ownership_with_executor(
        &self,
        room_id: &RoomId,
        new_owner_id: &UserId,
        executor: impl sqlx::PgExecutor<'_>,
    ) -> Result<Room> {
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            UPDATE rooms
            SET created_by = $2, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id AS "id: RoomId",
                      name,
                      description,
                      created_by AS "created_by: UserId",
                      closed_at,
                      false AS "is_banned!",
                      created_at,
                      updated_at,
                      deleted_at,
                      version,
                      last_activity_at
            "#,
            room_id.as_i64(),
            new_owner_id.as_i64(),
        )
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;

        Ok(room.into())
    }

    /// Touch the room's `last_activity_at` timestamp to reflect recent activity
    /// (chat messages, playback changes, member joins/leaves).
    ///
    /// This is a fire-and-forget operation -- callers should not block on it.
    pub async fn touch_activity(&self, room_id: &RoomId) -> Result<()> {
        sqlx::query!(
            "UPDATE rooms SET last_activity_at = CURRENT_TIMESTAMP WHERE id = $1 AND deleted_at IS NULL",
            room_id as &RoomId,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Fetch all data needed by the join-room flow in a single query.
    ///
    /// Combines three lookups that were previously sequential:
    ///   1. `rooms` row (by id, not soft-deleted)
    ///   2. Ban check (`room_members` where banned_at IS NOT NULL)
    ///   3. Room settings + password hash (`room_settings`)
    ///
    /// Returns `None` if the room does not exist or is soft-deleted.
    pub async fn get_join_context(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<JoinRoomContext>> {
        let sql = format!(
            r"
            SELECT
                {ROOM_SELECT_COLUMNS},
                EXISTS(
                    SELECT 1 FROM room_member_bans rmb
                    WHERE rmb.room_id = r.id
                      AND rmb.user_id = $2
                      AND rmb.revoked_at IS NULL
                      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                ) AS user_is_banned,
                rs_settings.value  AS settings_json,
                rs_password.value  AS password_hash
            FROM rooms r
            LEFT JOIN room_settings rs_settings
                ON rs_settings.room_id = r.id AND rs_settings.key = '_settings'
            LEFT JOIN room_settings rs_password
                ON rs_password.room_id = r.id AND rs_password.key = 'password'
            WHERE r.id = $1 AND r.deleted_at IS NULL
            "
        );
        let row = sqlx::query(&sql)
            .bind(room_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        let Some(row) = row else { return Ok(None) };

        let room = Room::from_row(&row)?;
        let is_banned: bool = row.try_get("user_is_banned")?;

        // Deserialize settings from JSON, falling back to defaults
        let settings: RoomSettings = match row.try_get::<Option<String>, _>("settings_json")? {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                crate::Error::Internal(format!("Failed to deserialize room settings: {e}"))
            })?,
            None => RoomSettings::default(),
        };

        let password_hash: Option<String> = row.try_get("password_hash")?;

        Ok(Some(JoinRoomContext {
            room,
            is_banned,
            settings,
            password_hash,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core_testing::create_test_pool;

    #[test]
    fn test_build_room_list_conditions_no_filters() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, next_idx) = wb.build(1);
        assert_eq!(sql, "r.deleted_at IS NULL");
        assert_eq!(next_idx, 1); // no params consumed
        assert_eq!(wb.param_count(), 0);
    }

    #[test]
    fn test_build_room_list_conditions_with_status_active() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Active),
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.deleted_at IS NULL"));
        assert!(sql.contains("r.closed_at IS NULL"));
        assert_eq!(wb.param_count(), 0); // status is a literal, not a param
    }

    #[test]
    fn test_build_room_list_conditions_with_status_closed_contains_closed_at_filter() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Closed),
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert_eq!(sql, "r.deleted_at IS NULL AND r.closed_at IS NOT NULL");
    }

    #[test]
    fn test_build_room_list_conditions_with_status_closed() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Closed),
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.closed_at IS NOT NULL"));
    }

    #[test]
    fn test_build_room_list_conditions_with_banned_filter() {
        let query = RoomListQuery {
            status: None,
            is_banned: Some(true),
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("room_bans rb"));
        assert!(sql.contains("rb.room_id = r.id"));
        assert!(sql.contains("rb.revoked_at IS NULL"));
    }

    #[test]
    fn test_build_room_list_conditions_with_not_banned_filter() {
        let query = RoomListQuery {
            status: None,
            is_banned: Some(false),
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("NOT EXISTS"));
        assert!(sql.contains("room_bans rb"));
        assert!(sql.contains("rb.room_id = r.id"));
    }

    #[test]
    fn test_build_room_list_conditions_with_search() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: Some("test".to_string()),
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, next_idx) = wb.build(1);
        assert!(sql.contains("r.name ILIKE"));
        assert!(sql.contains("r.description ILIKE"));
        assert_eq!(next_idx, 2);
    }

    #[test]
    fn test_build_room_list_conditions_all_filters() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Active),
            is_banned: Some(false),
            search: Some("room".to_string()),
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.deleted_at IS NULL"));
        assert!(sql.contains("r.closed_at IS NULL"));
        assert!(sql.contains("NOT EXISTS"));
        assert!(sql.contains("room_bans rb"));
        assert!(sql.contains("r.name ILIKE"));
    }

    #[test]
    fn test_build_room_list_conditions_param_offset_for_paginated_query() {
        // Simulates the list query where $1=LIMIT, $2=OFFSET, filters start at $3
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: Some("test".to_string()),
            pagination: PageParams::default(),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (count_sql, count_next) = wb.build(1);
        let (list_sql, list_next) = wb.build(3);

        // Count query uses $1 for the search param
        assert!(count_sql.contains("$1"));
        assert_eq!(count_next, 2);

        // List query uses $3 for the search param (after LIMIT $1 and OFFSET $2)
        assert!(list_sql.contains("$3"));
        assert_eq!(list_next, 4);
    }

    #[test]
    fn test_room_list_order_clause_supports_name_ascending() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: None,
            sort_by: crate::models::RoomListSortBy::Name,
            sort_direction: crate::models::SortDirection::Asc,
            pagination: PageParams::default(),
            creator_id: None,
        };

        assert_eq!(
            RoomRepository::build_order_by(&query),
            "r.name ASC, r.id ASC"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_room() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner user first (rooms have FK to users)
        let owner = UserFixture::new().with_username("room_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Test Room")
            .with_description("desc")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert_eq!(created.name, "Test Room");
        assert_eq!(created.created_by, owner.id);
        assert!(room_repo.exists(&created.id).await.unwrap());
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_room_duplicate_name_for_same_owner_returns_already_exists() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        let owner = user_repo
            .create(&UserFixture::new().with_username("room_dup_owner1").build())
            .await
            .unwrap();

        let room1 = RoomFixture::new()
            .with_name("Duplicate Room Name")
            .with_owner(owner.id)
            .build();
        room_repo.create(&room1).await.unwrap();

        let room2 = RoomFixture::new()
            .with_name("Duplicate Room Name")
            .with_owner(owner.id)
            .build();
        let result = room_repo.create(&room2).await;

        assert!(matches!(
            result,
            Err(crate::Error::AlreadyExists(ref msg))
                if msg == "You already have a room with this name"
        ));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_room_duplicate_name_for_different_owner_succeeds() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        let owner1 = user_repo
            .create(
                &UserFixture::new()
                    .with_username("room_shared_owner1")
                    .build(),
            )
            .await
            .unwrap();
        let owner2 = user_repo
            .create(
                &UserFixture::new()
                    .with_username("room_shared_owner2")
                    .build(),
            )
            .await
            .unwrap();

        let room1 = RoomFixture::new()
            .with_name("Shared Room Name")
            .with_owner(owner1.id)
            .build();
        let room2 = RoomFixture::new()
            .with_name("Shared Room Name")
            .with_owner(owner2.id)
            .build();

        room_repo.create(&room1).await.unwrap();
        let created = room_repo.create(&room2).await.unwrap();
        assert_eq!(created.name, "Shared Room Name");
        assert_eq!(created.created_by, owner2.id);
    }

    /// Unit test: Room model methods
    #[test]
    fn test_room_model_new() {
        let creator_id = UserId::new();
        let room = Room::new("My Room".to_string(), creator_id);

        assert_eq!(room.name, "My Room");
        assert!(room.description.is_empty());
        assert_eq!(room.created_by, creator_id);
        assert_eq!(room.status, RoomStatus::Active);
        assert!(!room.is_banned);
        assert!(room.deleted_at.is_none());
        assert!(room.is_active());
    }

    /// Unit test: `Room::new_with_description`
    #[test]
    fn test_room_model_new_with_description() {
        let creator_id = UserId::new();
        let room = Room::new_with_description(
            "My Room".to_string(),
            "A test room".to_string(),
            creator_id,
        );

        assert_eq!(room.name, "My Room");
        assert_eq!(room.description, "A test room");
        assert_eq!(room.created_by, creator_id);
    }

    /// Unit test: Room ban/unban
    #[test]
    fn test_room_ban_unban() {
        let creator_id = UserId::new();
        let mut room = Room::new("Test".to_string(), creator_id);

        assert!(!room.is_banned());
        assert!(room.is_active());

        room.ban();
        assert!(room.is_banned());
        assert!(room.is_active()); // Ban is independent from lifecycle state.

        room.unban();
        assert!(!room.is_banned());
        assert!(room.is_active()); // Active again after unban
    }

    /// Unit test: `RoomStatus` enum
    #[test]
    fn test_room_status() {
        assert_eq!(RoomStatus::Active.as_str(), "active");
        assert_eq!(RoomStatus::Closed.as_str(), "closed");

        assert!(RoomStatus::Active.is_active());
        assert!(RoomStatus::Closed.is_closed());
    }

    /// Unit test: `Room::is_active()` with various states
    #[test]
    fn test_room_is_active_combinations() {
        let creator_id = UserId::new();

        // Active status, not banned, not deleted
        let mut room = Room::new("Test".to_string(), creator_id);
        assert!(room.is_active());

        // Ban does not change lifecycle state.
        room.is_banned = true;
        assert!(room.is_active());

        // Not banned but closed
        room.is_banned = false;
        room.close();
        assert!(!room.is_active());

        // Deleted
        room.reopen();
        room.deleted_at = Some(chrono::Utc::now());
        assert!(!room.is_active());
    }

    /// Integration test: Get non-existent room returns None
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_nonexistent_room() {
        let (_postgres, pool) = create_test_pool().await;
        let room_repo = RoomRepository::new(pool.clone());

        let room_id = RoomId::expect_positive(92_001);
        let result = room_repo.get_by_id(&room_id).await.unwrap();
        assert!(result.is_none());
    }

    /// Integration test: Update room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_room() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("update_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Original Name")
            .with_description("Original description")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Update room
        let mut updated = created.clone();
        updated.name = "Updated Name".to_string();
        updated.description = "Updated description".to_string();

        let result = room_repo.update(&updated, created.version).await.unwrap();
        assert_eq!(result.name, "Updated Name");
        assert_eq!(result.description, "Updated description");
    }

    /// Integration test: Soft delete room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_soft_delete_room() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("delete_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room to Delete")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Soft delete
        let deleted = room_repo.delete(&created.id).await.unwrap();
        assert!(deleted);

        // Verify soft deleted (get_by_id returns None because deleted_at IS NOT NULL)
        let result = room_repo.get_by_id(&created.id).await.unwrap();
        assert!(result.is_none());

        // exists() also returns false
        let exists = room_repo.exists(&created.id).await.unwrap();
        assert!(!exists);

        // Delete again returns false
        let deleted_again = room_repo.delete(&created.id).await.unwrap();
        assert!(!deleted_again);
    }

    /// Integration test: Hard delete room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_hard_delete_room() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new()
            .with_username("hard_delete_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room to Hard Delete")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Hard delete
        let deleted = room_repo.hard_delete(&created.id).await.unwrap();
        assert!(deleted);
    }

    /// Integration test: Update room status
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_room_status() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("status_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Status Test Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert_eq!(created.status, RoomStatus::Active);

        // Update to Closed
        let updated = room_repo
            .update_status(&created.id, RoomStatus::Closed)
            .await
            .unwrap();
        assert_eq!(updated.status, RoomStatus::Closed);
    }

    /// Integration test: Update room ban status
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_ban_status() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("ban_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Ban Test Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert!(!created.is_banned);

        // Ban room
        let updated = room_repo
            .update_ban_status(&created.id, true)
            .await
            .unwrap();
        assert!(updated.is_banned);

        // Unban room
        let updated = room_repo
            .update_ban_status(&created.id, false)
            .await
            .unwrap();
        assert!(!updated.is_banned);
    }

    /// Integration test: Update room description
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_description() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("desc_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Desc Test Room")
            .with_description("Original description")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Update description
        let updated = room_repo
            .update_description(&created.id, "New description")
            .await
            .unwrap();
        assert_eq!(updated.description, "New description");
    }

    /// Integration test: List rooms with pagination
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_rooms_pagination() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("list_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create 15 rooms
        for i in 0..15 {
            let room = RoomFixture::new()
                .with_name(&format!("List Room {i}"))
                .with_owner(owner.id)
                .build();
            room_repo.create(&room).await.unwrap();
        }

        // List with pagination
        let query = RoomListQuery {
            pagination: PageParams::new(Some(1), Some(10)),
            status: None,
            search: None,
            is_banned: None,
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let (rooms, total) = room_repo.list(&query).await.unwrap();
        assert_eq!(rooms.len(), 10);
        assert_eq!(total, 15);

        // Second page
        let query = RoomListQuery {
            pagination: PageParams::new(Some(2), Some(10)),
            status: None,
            search: None,
            is_banned: None,
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let (rooms, total) = room_repo.list(&query).await.unwrap();
        assert_eq!(rooms.len(), 5);
        assert_eq!(total, 15);
    }

    /// Integration test: List rooms with filters
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_rooms_with_filters() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("filter_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create active room
        let room = RoomFixture::new()
            .with_name("Active Room")
            .with_owner(owner.id)
            .build();
        room_repo.create(&room).await.unwrap();

        // Create and ban a room
        let mut banned_room = RoomFixture::new()
            .with_name("Banned Room")
            .with_owner(owner.id)
            .build();
        banned_room.is_banned = true;
        room_repo.create(&banned_room).await.unwrap();

        // Filter by status Active
        let query = RoomListQuery {
            pagination: PageParams::default(),
            status: Some(RoomStatus::Active),
            search: None,
            is_banned: None,
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let (rooms, _) = room_repo.list(&query).await.unwrap();
        assert!(rooms.iter().all(|r| r.status == RoomStatus::Active));

        // Filter by not banned
        let query = RoomListQuery {
            pagination: PageParams::default(),
            status: None,
            search: None,
            is_banned: Some(false),
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let (rooms, _) = room_repo.list(&query).await.unwrap();
        assert!(rooms.iter().all(|r| !r.is_banned));

        // Filter by search term
        let query = RoomListQuery {
            pagination: PageParams::default(),
            status: None,
            search: Some("Active".to_string()),
            is_banned: None,
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let (rooms, _) = room_repo.list(&query).await.unwrap();
        assert!(rooms.iter().all(|r| r.name.contains("Active")));
    }

    /// Integration test: room member_count excludes banned and departed members.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_with_count_excludes_banned_and_departed_members() {
        use crate::models::{RoomMember, RoomRole, User};
        use crate::repository::{RoomMemberRepository, UserRepository};
        use crate::test_helpers::{RoomFixture, UserFixture};
        use chrono::Utc;

        fn make_user(username: &str) -> User {
            User::new(
                username.to_string(),
                Some(format!("{username}@test.com")),
                "hash".to_string(),
                crate::models::SignupMethod::Email,
            )
        }

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());
        let member_repo = RoomMemberRepository::new(pool.clone());

        let owner = user_repo
            .create(&UserFixture::new().with_username("count_owner").build())
            .await
            .unwrap();
        let active = user_repo.create(&make_user("count_active")).await.unwrap();
        let banned = user_repo.create(&make_user("count_banned")).await.unwrap();
        let rejected = user_repo
            .create(&make_user("count_rejected"))
            .await
            .unwrap();

        let room = room_repo
            .create(
                &RoomFixture::new()
                    .with_name("Counted Room")
                    .with_owner(owner.id)
                    .build(),
            )
            .await
            .unwrap();

        member_repo
            .add(&RoomMember::new(room.id, active.id, RoomRole::Member))
            .await
            .unwrap();

        member_repo
            .add(&RoomMember {
                ..RoomMember::new(room.id, banned.id, RoomRole::Member)
            })
            .await
            .unwrap();
        member_repo
            .ban_member(
                &room.id,
                &banned.id,
                Some(&owner.id),
                Some("count test".to_string()),
            )
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version
             ) VALUES ($1, $2, $3, 0, 0, 0, 0, $4, $5, 0)",
        )
        .bind(room.id.as_i64())
        .bind(rejected.id.as_i64())
        .bind(i16::try_from(i32::from(RoomRole::Member)).unwrap())
        .bind(Utc::now())
        .bind(Some(Utc::now()))
        .execute(&pool)
        .await
        .unwrap();

        let query = RoomListQuery {
            pagination: PageParams::new(Some(1), Some(10)),
            status: None,
            search: Some("Counted".to_string()),
            is_banned: None,
            creator_id: None,
            sort_by: crate::models::RoomListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };

        let (rows, total) = room_repo.list_with_count(&query).await.unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].room.id, room.id);
        assert_eq!(
            rows[0].member_count, 1,
            "room member_count should exclude banned/departed rows"
        );
        assert_eq!(room_repo.get_member_count(&room.id).await.unwrap(), 1);
    }

    /// Integration test: List rooms by creator
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_by_creator() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create two users
        let owner1 = UserFixture::new().with_username("creator1").build();
        let owner1 = user_repo.create(&owner1).await.unwrap();

        let owner2 = UserFixture::new().with_username("creator2").build();
        let owner2 = user_repo.create(&owner2).await.unwrap();

        // Create rooms for owner1
        for i in 0..3 {
            let room = RoomFixture::new()
                .with_name(&format!("Owner1 Room {i}"))
                .with_owner(owner1.id)
                .build();
            room_repo.create(&room).await.unwrap();
        }

        // Create rooms for owner2
        for i in 0..2 {
            let room = RoomFixture::new()
                .with_name(&format!("Owner2 Room {i}"))
                .with_owner(owner2.id)
                .build();
            room_repo.create(&room).await.unwrap();
        }

        // List by creator
        let (rooms, total) = room_repo
            .list_by_creator(&owner1.id, PageParams::default())
            .await
            .unwrap();
        assert_eq!(rooms.len(), 3);
        assert_eq!(total, 3);

        let (rooms, total) = room_repo
            .list_by_creator(&owner2.id, PageParams::default())
            .await
            .unwrap();
        assert_eq!(rooms.len(), 2);
        assert_eq!(total, 2);
    }

    /// Integration test: `is_accessible`
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_is_accessible() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("accessible_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create active room
        let room = RoomFixture::new()
            .with_name("Accessible Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Active room is accessible
        assert!(room_repo.is_accessible(&created.id).await.unwrap());

        // Ban room
        room_repo
            .update_ban_status(&created.id, true)
            .await
            .unwrap();
        assert!(!room_repo.is_accessible(&created.id).await.unwrap());

        // Unban and close
        room_repo
            .update_ban_status(&created.id, false)
            .await
            .unwrap();
        room_repo
            .update_status(&created.id, RoomStatus::Closed)
            .await
            .unwrap();
        assert!(!room_repo.is_accessible(&created.id).await.unwrap());
    }

    /// Integration test: `get_join_context`
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_join_context() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new()
            .with_username("join_context_owner")
            .build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Join Context Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Get join context
        let context = room_repo
            .get_join_context(&created.id, &owner.id)
            .await
            .unwrap();
        assert!(context.is_some());

        let context = context.unwrap();
        assert_eq!(context.room.id, created.id);
        assert!(!context.is_banned); // Owner is not banned

        // Non-existent room returns None
        let non_existent = RoomId::expect_positive(92_002);
        let context = room_repo
            .get_join_context(&non_existent, &owner.id)
            .await
            .unwrap();
        assert!(context.is_none());
    }

    /// Integration test: `create_with_executor`
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_with_executor() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("executor_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create room with executor (pool)
        let room = RoomFixture::new()
            .with_name("Executor Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create_with_executor(&room, &pool).await.unwrap();
        assert_eq!(created.name, "Executor Room");
    }

    /// Integration test: Room not found error handling
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_nonexistent_room() {
        let (_postgres, pool) = create_test_pool().await;
        let room_repo = RoomRepository::new(pool.clone());

        // Try to update non-existent room
        let room = Room::new("Non-existent".to_string(), UserId::new());
        let result = room_repo.update(&room, 0).await;
        assert!(matches!(result, Err(crate::Error::NotFound(_))));
    }

    /// Integration test: Optimistic lock conflict
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_stale_version_returns_optimistic_lock_conflict() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("optimistic_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Optimistic Room")
            .with_description("original")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();
        let original_version = created.version;

        // First update succeeds
        let mut updated_room = created.clone();
        updated_room.name = "Updated Name V1".to_string();
        updated_room.description = "updated v1".to_string();
        let v1 = room_repo
            .update(&updated_room, original_version)
            .await
            .unwrap();
        assert_eq!(v1.version, original_version + 1);
        assert_eq!(v1.name, "Updated Name V1");

        // Second update with stale version (original_version) -> should get OptimisticLockConflict
        let mut stale_room = created.clone();
        stale_room.name = "Updated Name V2".to_string();
        stale_room.description = "updated v2".to_string();
        let err = room_repo
            .update(&stale_room, original_version)
            .await
            .unwrap_err();
        assert!(
            matches!(err, crate::Error::OptimisticLockConflict),
            "Expected OptimisticLockConflict, got: {err:?}"
        );
    }

    /// Integration test: Update soft-deleted room returns `NotFound`
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_soft_deleted_room_returns_not_found() {
        use crate::repository::user::UserRepository;
        use crate::test_helpers::{RoomFixture, UserFixture};

        let (_postgres, pool) = create_test_pool().await;
        let user_repo = UserRepository::new(pool.clone());
        let room_repo = RoomRepository::new(pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("softdel_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Soft Delete Room")
            .with_owner(owner.id)
            .build();
        let created = room_repo.create(&room).await.unwrap();
        let version = created.version;

        // Soft delete the room
        let deleted = room_repo.delete(&created.id).await.unwrap();
        assert!(deleted);

        // Trying to update the deleted room should return NotFound (not OptimisticLockConflict)
        let mut updated = created.clone();
        updated.name = "Updated Soft Deleted".to_string();
        let err = room_repo.update(&updated, version).await.unwrap_err();
        assert!(
            matches!(err, crate::Error::NotFound(_)),
            "Expected NotFound for soft-deleted room, got: {err:?}"
        );
    }
}
