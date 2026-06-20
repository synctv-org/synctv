use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

use super::{query_builder::escape_ilike, required_count};
use crate::{
    models::{
        OpaquePasswordRecord, PageParams, Room, RoomId, RoomListQuery, RoomListSortBy,
        RoomSettings, RoomStatus, UserId,
    },
    Error, Result,
};

#[derive(Debug, sqlx::FromRow)]
struct RoomRow {
    id: RoomId,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
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
            cover_file_reference_id: row.cover_file_reference_id,
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

#[derive(Debug, sqlx::FromRow)]
struct RoomWithCountRow {
    id: RoomId,
    name: String,
    description: String,
    cover_file_reference_id: Option<i64>,
    created_by: UserId,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    is_banned: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    version: i32,
    last_activity_at: chrono::DateTime<chrono::Utc>,
    member_count: i32,
}

impl From<RoomWithCountRow> for crate::models::RoomWithCount {
    fn from(row: RoomWithCountRow) -> Self {
        let member_count = row.member_count;
        let room = RoomRow {
            id: row.id,
            name: row.name,
            description: row.description,
            cover_file_reference_id: row.cover_file_reference_id,
            created_by: row.created_by,
            closed_at: row.closed_at,
            is_banned: row.is_banned,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
            version: row.version,
            last_activity_at: row.last_activity_at,
        }
        .into();
        Self { room, member_count }
    }
}
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
/// Pre-fetched context for the join-room flow, retrieved in a single DB round-trip.
#[derive(Debug)]
pub struct JoinRoomContext {
    pub room: Room,
    pub is_in_kick_cooldown: bool,
    pub settings: RoomSettings,
    pub password_enabled: bool,
    pub password_credential: Option<OpaquePasswordRecord>,
    pub password_version: Option<i32>,
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

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create a new room.
    ///
    /// Product policies such as duplicate-name handling belong in the service
    /// layer; this repository method only persists a validated room row.
    pub async fn create(&self, room: &Room) -> Result<Room> {
        self.create_with_executor(room, &self.pool).await
    }

    /// Create a new room using a provided executor (pool or transaction).
    ///
    /// Product policies such as duplicate-name handling belong in the service
    /// layer; this repository method only persists a validated room row.
    pub async fn create_with_executor<'e, E>(&self, room: &Room, executor: E) -> Result<Room>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let created = sqlx::query_as!(
            RoomRow,
            r#"
             INSERT INTO rooms (name, description, cover_file_reference_id,
                                created_by, closed_at, created_at, updated_at, version, last_activity_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING id AS "id: RoomId",
                       name,
                       description,
                       cover_file_reference_id,
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
            room.cover_file_reference_id,
            room.created_by.as_i64(),
            room.closed_at,
            room.created_at,
            room.updated_at,
            room.version,
            room.last_activity_at
        )
        .fetch_one(executor)
        .await?;

        Ok(created.into())
    }

    pub async fn active_name_exists_for_creator(
        &self,
        creator_id: &UserId,
        name: &str,
    ) -> Result<bool> {
        Self::active_name_exists_for_creator_with_executor(creator_id, name, &self.pool).await
    }

    pub async fn active_name_exists_for_creator_with_executor<'e, E>(
        creator_id: &UserId,
        name: &str,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM rooms
                WHERE created_by = $1
                  AND name = $2
                  AND deleted_at IS NULL
            ) AS "exists!"
            "#,
            creator_id as &UserId,
            name
        )
        .fetch_one(executor)
        .await?;

        Ok(exists)
    }

    /// Get room by ID
    pub async fn get_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
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
            room_id as &RoomId
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(room.map(Into::into))
    }

    pub async fn get_by_id_for_update_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        executor: E,
    ) -> Result<Option<Room>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let room = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
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
            FOR UPDATE
            "#,
            room_id as &RoomId
        )
        .fetch_optional(executor)
        .await?;

        Ok(room.map(Into::into))
    }

    /// Get active, non-banned rooms by ID.
    pub async fn list_active_unbanned_by_ids(&self, room_ids: &[RoomId]) -> Result<Vec<Room>> {
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();
        let rows = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
                   r.created_by AS "created_by: UserId",
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
                   ) AS "is_banned!"
            FROM rooms r
            WHERE r.id = ANY($1)
              AND r.deleted_at IS NULL
              AND r.closed_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM room_bans rb
                  WHERE rb.room_id = r.id
                    AND rb.revoked_at IS NULL
                    AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
              )
            "#,
            &ids
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(Into::into).collect())
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
        self.update_with_executor(room, old_version, &self.pool)
            .await
    }

    pub async fn update_with_executor<'e, E>(
        &self,
        room: &Room,
        old_version: i32,
        executor: E,
    ) -> Result<Room>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let updated = sqlx::query_as!(
            RoomRow,
            r#"
             WITH updated AS (
                 UPDATE rooms
                 SET name = $2, description = $3,
                     cover_file_reference_id = $4,
                     closed_at = $5,
                     version = version + 1
                 WHERE id = $1 AND deleted_at IS NULL AND version = $6
                 RETURNING id,
                           name,
                           description,
                           cover_file_reference_id,
                           created_by,
                           closed_at,
                           created_at,
                           updated_at,
                           deleted_at,
                           version,
                           last_activity_at
             )
             SELECT u.id AS "id!: RoomId",
                    u.name,
                    u.description,
                    u.cover_file_reference_id,
                    u.created_by AS "created_by!: UserId",
                    u.closed_at,
                    EXISTS (
                        SELECT 1 FROM room_bans rb
                        WHERE rb.room_id = u.id
                          AND rb.revoked_at IS NULL
                          AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                    ) AS "is_banned!",
                    u.created_at,
                    u.updated_at,
                    u.deleted_at,
                    u.version,
                    u.last_activity_at
             FROM updated u
            "#,
            room.id.as_i64(),
            &room.name,
            &room.description,
            room.cover_file_reference_id,
            room.closed_at,
            old_version
        )
        .fetch_optional(executor)
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
            super::room_cleanup::hard_delete_room_and_cleanup_in_tx(&mut tx, room_id).await?;
        tx.commit().await?;
        Ok(deleted)
    }

    fn push_room_projection(builder: &mut QueryBuilder<Postgres>) {
        builder.push(
            r"
            r.id,
            r.name,
            r.description,
            r.cover_file_reference_id,
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
            ",
        );
    }

    fn push_where_prefix(builder: &mut QueryBuilder<Postgres>, has_condition: &mut bool) {
        if *has_condition {
            builder.push(" AND ");
        } else {
            builder.push(" WHERE ");
            *has_condition = true;
        }
    }

    fn push_room_list_filters<'q>(
        builder: &mut QueryBuilder<Postgres>,
        query: &'q RoomListQuery,
        search_pattern: Option<&'q String>,
        has_condition: &mut bool,
    ) {
        Self::push_where_prefix(builder, has_condition);
        builder.push("r.deleted_at IS NULL");

        match query.status {
            Some(RoomStatus::Active) => {
                Self::push_where_prefix(builder, has_condition);
                builder.push("r.closed_at IS NULL");
            }
            Some(RoomStatus::Closed) => {
                Self::push_where_prefix(builder, has_condition);
                builder.push("r.closed_at IS NOT NULL");
            }
            None => {}
        }

        match query.is_banned {
            Some(true) => {
                Self::push_where_prefix(builder, has_condition);
                builder.push(ACTIVE_ROOM_BAN_EXISTS_SQL);
            }
            Some(false) => {
                Self::push_where_prefix(builder, has_condition);
                builder.push(ACTIVE_ROOM_BAN_NOT_EXISTS_SQL);
            }
            None => {}
        }

        if let Some(pattern) = search_pattern {
            Self::push_where_prefix(builder, has_condition);
            builder
                .push("(r.name ILIKE ")
                .push_bind(pattern)
                .push(" OR r.description ILIKE ")
                .push_bind(pattern)
                .push(")");
        }

        if let Some(creator_id) = &query.creator_id {
            Self::push_where_prefix(builder, has_condition);
            builder.push("r.created_by = ").push_bind(creator_id);
        }
    }

    fn order_by_sql(query: &RoomListQuery) -> &'static str {
        use crate::models::SortDirection;

        match (query.sort_by, query.sort_direction) {
            (RoomListSortBy::Name, SortDirection::Asc) => "r.name ASC, r.id ASC",
            (RoomListSortBy::Name, SortDirection::Desc) => "r.name DESC, r.id DESC",
            (RoomListSortBy::UpdatedAt, SortDirection::Asc) => "r.updated_at ASC, r.id ASC",
            (RoomListSortBy::UpdatedAt, SortDirection::Desc) => "r.updated_at DESC, r.id DESC",
            (RoomListSortBy::LastActivityAt, SortDirection::Asc) => {
                "r.last_activity_at ASC NULLS LAST, r.id ASC"
            }
            (RoomListSortBy::LastActivityAt, SortDirection::Desc) => {
                "r.last_activity_at DESC NULLS LAST, r.id DESC"
            }
            (RoomListSortBy::CreatedAt, SortDirection::Asc) => "r.created_at ASC, r.id ASC",
            (RoomListSortBy::CreatedAt, SortDirection::Desc) => "r.created_at DESC, r.id DESC",
        }
    }

    /// List rooms with pagination and filters
    pub async fn list(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let mut count_builder = QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM rooms r");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut count_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        let count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        let mut list_builder = QueryBuilder::<Postgres>::new("SELECT ");
        Self::push_room_projection(&mut list_builder);
        list_builder.push(" FROM rooms r");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut list_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        list_builder
            .push(" ORDER BY ")
            .push(Self::order_by_sql(query))
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let rooms = list_builder
            .build_query_as::<RoomRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok((rooms, count))
    }

    /// List only rooms whose creator is still active.
    pub async fn list_accessible(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let mut count_builder = QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM rooms r");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut count_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        Self::push_where_prefix(&mut count_builder, &mut has_condition);
        count_builder.push(ACCESSIBLE_ROOM_CREATOR_CONDITION);
        let count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        let mut list_builder = QueryBuilder::<Postgres>::new("SELECT ");
        Self::push_room_projection(&mut list_builder);
        list_builder.push(" FROM rooms r");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut list_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        Self::push_where_prefix(&mut list_builder, &mut has_condition);
        list_builder.push(ACCESSIBLE_ROOM_CREATOR_CONDITION);
        list_builder
            .push(" ORDER BY ")
            .push(Self::order_by_sql(query))
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let rooms = list_builder
            .build_query_as::<RoomRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok((rooms, count))
    }

    /// List rooms related to a user, either by ownership or active membership.
    pub async fn list_related_to_user(
        &self,
        user_id: &UserId,
        query: &RoomListQuery,
    ) -> Result<(Vec<Room>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));

        let mut count_builder = QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM rooms r");
        let mut has_condition = false;
        Self::push_where_prefix(&mut count_builder, &mut has_condition);
        count_builder
            .push("(r.created_by = ")
            .push_bind(user_id)
            .push(
                " OR EXISTS (
                SELECT 1
                FROM room_members rm
                WHERE rm.room_id = r.id
                  AND rm.user_id = ",
            )
            .push_bind(user_id)
            .push("))");
        Self::push_room_list_filters(
            &mut count_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        let count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        let mut list_builder = QueryBuilder::<Postgres>::new("SELECT ");
        Self::push_room_projection(&mut list_builder);
        list_builder.push(" FROM rooms r");
        let mut has_condition = false;
        Self::push_where_prefix(&mut list_builder, &mut has_condition);
        list_builder
            .push("(r.created_by = ")
            .push_bind(user_id)
            .push(
                " OR EXISTS (
                SELECT 1
                FROM room_members rm
                WHERE rm.room_id = r.id
                  AND rm.user_id = ",
            )
            .push_bind(user_id)
            .push("))");
        Self::push_room_list_filters(
            &mut list_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        list_builder
            .push(" ORDER BY ")
            .push(Self::order_by_sql(query))
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let rooms = list_builder
            .build_query_as::<RoomRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok((rooms, count))
    }

    /// List rooms with member count (optimized with JOIN)
    pub async fn list_with_count(
        &self,
        query: &RoomListQuery,
    ) -> Result<(Vec<crate::models::RoomWithCount>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let mut count_builder =
            QueryBuilder::<Postgres>::new("SELECT COUNT(DISTINCT r.id) FROM rooms r");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut count_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        let count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        let mut list_builder = QueryBuilder::<Postgres>::new("SELECT ");
        Self::push_room_projection(&mut list_builder);
        list_builder.push(", COALESCE(COUNT(rm.user_id), 0)::int AS member_count");
        list_builder.push(" FROM rooms r LEFT JOIN room_members rm ON r.id = rm.room_id");
        let mut has_condition = false;
        Self::push_room_list_filters(
            &mut list_builder,
            query,
            search_pattern.as_ref(),
            &mut has_condition,
        );
        list_builder.push(
            " GROUP BY r.id, r.name, r.description, r.cover_file_reference_id, r.created_by,
              r.closed_at, r.created_at, r.updated_at, r.deleted_at, r.version,
              r.last_activity_at",
        );
        list_builder
            .push(" ORDER BY ")
            .push(Self::order_by_sql(query))
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);

        let rooms_with_count = list_builder
            .build_query_as::<RoomWithCountRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(Into::into)
            .collect();

        Ok((rooms_with_count, count))
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

    /// Get room member count.
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        let count = required_count(
            sqlx::query_scalar!(
                r"
            SELECT COUNT(*) as count
            FROM room_members rm
            WHERE rm.room_id = $1
            ",
                room_id as &RoomId,
            )
            .fetch_one(&self.pool)
            .await?,
            "room member",
        )?;

        count_i64_to_i32(count)
    }

    /// Get rooms created by a specific user
    pub async fn list_by_creator(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<Room>, i64)> {
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

        let count = required_count(
            sqlx::query_scalar!(
                "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL",
                creator_id as &UserId,
            )
            .fetch_one(&self.pool)
            .await?,
            "rooms by creator",
        )?;

        let rooms = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
                   r.created_by AS "created_by: UserId",
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
                   ) AS "is_banned!"
            FROM rooms r
            WHERE r.created_by = $1 AND r.deleted_at IS NULL
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            "#,
            creator_id as &UserId,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();

        Ok((rooms, count))
    }

    /// Get rooms created by a specific user with member count (optimized)
    pub async fn list_by_creator_with_count(
        &self,
        creator_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<crate::models::RoomWithCount>, i64)> {
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

        let count = required_count(
            sqlx::query_scalar!(
                "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL",
                creator_id as &UserId,
            )
            .fetch_one(&self.pool)
            .await?,
            "rooms by creator with count",
        )?;

        let rows = sqlx::query!(
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
                   r.created_by AS "created_by: UserId",
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
                   ) AS "is_banned!",
                   COALESCE(COUNT(rm.user_id), 0)::int AS "member_count!"
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE r.created_by = $1 AND r.deleted_at IS NULL
            GROUP BY r.id, r.name, r.description, r.cover_file_reference_id, r.created_by,
                     r.closed_at, r.created_at, r.updated_at, r.deleted_at, r.version,
                     r.last_activity_at
            ORDER BY r.created_at DESC
            LIMIT $2 OFFSET $3
            "#,
            creator_id as &UserId,
            limit,
            offset
        )
        .fetch_all(&self.pool)
        .await?;

        let rooms_with_count = rows
            .into_iter()
            .map(|row| crate::models::RoomWithCount {
                room: RoomRow {
                    id: row.id,
                    name: row.name,
                    description: row.description,
                    cover_file_reference_id: row.cover_file_reference_id,
                    created_by: row.created_by,
                    closed_at: row.closed_at,
                    is_banned: row.is_banned,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                    deleted_at: row.deleted_at,
                    version: row.version,
                    last_activity_at: row.last_activity_at,
                }
                .into(),
                member_count: row.member_count,
            })
            .collect();

        Ok((rooms_with_count, count))
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
            WITH updated AS (
                UPDATE rooms
                SET closed_at = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
                WHERE id = $2 AND deleted_at IS NULL
                RETURNING id,
                          name,
                          description,
                          cover_file_reference_id,
                          created_by,
                          closed_at,
                          created_at,
                          updated_at,
                          deleted_at,
                          version,
                          last_activity_at
            )
            SELECT u.id AS "id!: RoomId",
                   u.name,
                   u.description,
                   u.cover_file_reference_id,
                   u.created_by AS "created_by!: UserId",
                   u.closed_at,
                   EXISTS (
                       SELECT 1 FROM room_bans rb
                       WHERE rb.room_id = u.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS "is_banned!",
                   u.created_at,
                   u.updated_at,
                   u.deleted_at,
                   u.version,
                   u.last_activity_at
            FROM updated u
            "#,
            closed_at,
            room_id as &RoomId
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {room_id} not found")))?;
        Ok(room.into())
    }

    /// Update room ban policy using `room_bans`.
    pub async fn update_ban_status(&self, room_id: &RoomId, is_banned: bool) -> Result<Room> {
        let mut tx = self.pool.begin().await?;
        let room = Self::update_ban_status_with_executor(room_id, is_banned, &mut tx).await?;
        tx.commit().await?;
        Ok(room)
    }

    pub async fn update_ban_status_with_executor(
        room_id: &RoomId,
        is_banned: bool,
        executor: &mut PgConnection,
    ) -> Result<Room> {
        if is_banned {
            let lock_key = format!("room-ban:{room_id}");
            sqlx::query!(
                r"
                WITH _lock AS (
                    SELECT pg_advisory_xact_lock(hashtextextended($2, 0))
                )
                INSERT INTO room_bans (room_id, starts_at)
                SELECT r.id, CURRENT_TIMESTAMP
                FROM rooms r, _lock
                WHERE r.id = $1 AND r.deleted_at IS NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM room_bans rb
                      WHERE rb.room_id = r.id
                        AND rb.revoked_at IS NULL
                        AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                  )
                ",
                room_id as &RoomId,
                lock_key
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

        let room = sqlx::query_as!(
            RoomRow,
            r#"
            SELECT r.id AS "id: RoomId",
                   r.name,
                   r.description,
                   r.cover_file_reference_id,
                   r.created_by AS "created_by: UserId",
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
                   ) AS "is_banned!"
            FROM rooms r
            WHERE r.id = $1 AND r.deleted_at IS NULL
            "#,
            room_id as &RoomId
        )
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
            WITH updated AS (
                UPDATE rooms
                SET description = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
                WHERE id = $2 AND deleted_at IS NULL
                RETURNING id,
                          name,
                          description,
                          cover_file_reference_id,
                          created_by,
                          closed_at,
                          created_at,
                          updated_at,
                          deleted_at,
                          version,
                          last_activity_at
            )
            SELECT u.id AS "id!: RoomId",
                   u.name,
                   u.description,
                   u.cover_file_reference_id,
                   u.created_by AS "created_by!: UserId",
                   u.closed_at,
                   EXISTS (
                       SELECT 1 FROM room_bans rb
                       WHERE rb.room_id = u.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS "is_banned!",
                   u.created_at,
                   u.updated_at,
                   u.deleted_at,
                   u.version,
                   u.last_activity_at
            FROM updated u
            "#,
            description,
            room_id as &RoomId
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
            WITH updated AS (
                UPDATE rooms
                SET created_by = $2, updated_at = CURRENT_TIMESTAMP, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING id,
                          name,
                          description,
                          cover_file_reference_id,
                          created_by,
                          closed_at,
                          created_at,
                          updated_at,
                          deleted_at,
                          version,
                          last_activity_at
            )
            SELECT u.id AS "id!: RoomId",
                   u.name,
                   u.description,
                   u.cover_file_reference_id,
                   u.created_by AS "created_by!: UserId",
                   u.closed_at,
                   EXISTS (
                       SELECT 1 FROM room_bans rb
                       WHERE rb.room_id = u.id
                         AND rb.revoked_at IS NULL
                         AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
                   ) AS "is_banned!",
                   u.created_at,
                   u.updated_at,
                   u.deleted_at,
                   u.version,
                   u.last_activity_at
            FROM updated u
            "#,
            room_id as &RoomId,
            new_owner_id as &UserId
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
    /// Returns:
    ///   1. `rooms` row (by id, not soft-deleted)
    ///   2. Active kick cooldown check
    ///   3. Room settings + OPAQUE password credential
    ///
    /// Returns `None` if the room does not exist or is soft-deleted.
    pub async fn get_join_context(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<JoinRoomContext>> {
        let row = sqlx::query!(
            r#"
            SELECT r.id AS "id: RoomId",
                r.name,
                r.description,
                r.cover_file_reference_id,
                r.created_by AS "created_by: UserId",
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
                ) AS "is_banned!",
                EXISTS(
                    SELECT 1 FROM room_member_kick_cooldowns rmkc
                    WHERE rmkc.room_id = r.id
                      AND rmkc.user_id = $2
                      AND rmkc.ends_at > CURRENT_TIMESTAMP
                ) AS "is_in_kick_cooldown!",
                rs_settings.value AS "settings_json?: String",
                rpc.opaque_record,
                rpc.opaque_credential_identifier,
                rpc.opaque_ciphersuite,
                rpc.opaque_server_setup_version,
                COALESCE(rpc.enabled, false) AS "password_enabled!",
                rpc.version AS "password_version?"
            FROM rooms r
            LEFT JOIN room_settings rs_settings
                ON rs_settings.room_id = r.id AND rs_settings.key = '_settings'
            LEFT JOIN room_password_credentials rpc
                ON rpc.room_id = r.id
            WHERE r.id = $1 AND r.deleted_at IS NULL
            "#,
            room_id as &RoomId,
            user_id as &UserId
        )
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };

        let room = RoomRow {
            id: row.id,
            name: row.name,
            description: row.description,
            cover_file_reference_id: row.cover_file_reference_id,
            created_by: row.created_by,
            closed_at: row.closed_at,
            is_banned: row.is_banned,
            created_at: row.created_at,
            updated_at: row.updated_at,
            deleted_at: row.deleted_at,
            version: row.version,
            last_activity_at: row.last_activity_at,
        }
        .into();

        let settings: RoomSettings = match row.settings_json.as_deref() {
            Some(json) => serde_json::from_str(json).map_err(|e| {
                crate::Error::Internal(format!("Failed to deserialize room settings: {e}"))
            })?,
            None => RoomSettings::default(),
        };

        let password_credential = match (
            row.opaque_record,
            row.opaque_credential_identifier,
            row.opaque_ciphersuite,
            row.opaque_server_setup_version,
        ) {
            (Some(record), Some(credential_identifier), Some(ciphersuite), Some(version)) => {
                Some(OpaquePasswordRecord {
                    record,
                    credential_identifier,
                    ciphersuite,
                    server_setup_version: version,
                })
            }
            (None, None, None, None) => None,
            _ => {
                return Err(Error::Internal(
                    "Incomplete OPAQUE room password credential material".to_string(),
                ));
            }
        };

        Ok(Some(JoinRoomContext {
            room,
            is_in_kick_cooldown: row.is_in_kick_cooldown,
            settings,
            password_enabled: row.password_enabled,
            password_credential,
            password_version: row.password_version,
        }))
    }
}

#[cfg(test)]
#[path = "room_tests.rs"]
mod tests;
