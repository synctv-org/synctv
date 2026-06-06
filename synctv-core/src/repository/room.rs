use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

use super::query_builder::escape_ilike;
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

    fn push_room_projection(builder: &mut QueryBuilder<'_, Postgres>) {
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

    fn push_where_prefix(builder: &mut QueryBuilder<'_, Postgres>, has_condition: &mut bool) {
        if *has_condition {
            builder.push(" AND ");
        } else {
            builder.push(" WHERE ");
            *has_condition = true;
        }
    }

    fn push_room_list_filters<'q>(
        builder: &mut QueryBuilder<'q, Postgres>,
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
        let count = sqlx::query_scalar!(
            r"
            SELECT COUNT(*) as count
            FROM room_members rm
            WHERE rm.room_id = $1
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
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

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
mod tests {
    use super::*;
    use synctv_core_testing::create_test_pool;

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

        assert_eq!(RoomRepository::order_by_sql(&query), "r.name ASC, r.id ASC");
    }

    #[test]
    fn test_room_list_order_clause_supports_last_activity_nulls_last() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: None,
            sort_by: crate::models::RoomListSortBy::LastActivityAt,
            sort_direction: crate::models::SortDirection::Desc,
            pagination: PageParams::default(),
            creator_id: None,
        };

        assert_eq!(
            RoomRepository::order_by_sql(&query),
            "r.last_activity_at DESC NULLS LAST, r.id DESC"
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
    async fn test_create_room_duplicate_name_for_same_owner_is_repository_allowed() {
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

        let created = result.expect("repository should not enforce room-name product policy");
        assert_eq!(created.name, "Duplicate Room Name");
        assert_eq!(created.created_by, owner.id);
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

    #[test]
    fn test_room_status() {
        assert_eq!(RoomStatus::Active.as_str(), "active");
        assert_eq!(RoomStatus::Closed.as_str(), "closed");

        assert!(RoomStatus::Active.is_active());
        assert!(RoomStatus::Closed.is_closed());
    }

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

        let banned = room_repo
            .update_ban_status(&created.id, true)
            .await
            .unwrap();
        assert!(banned.is_banned);

        let reopened = room_repo
            .update_status(&created.id, RoomStatus::Active)
            .await
            .unwrap();
        assert_eq!(reopened.status, RoomStatus::Active);
        assert!(
            reopened.is_banned,
            "status updates must preserve the derived active room-ban state"
        );
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

        room_repo
            .update_ban_status(&created.id, true)
            .await
            .unwrap();
        let updated = room_repo
            .update_description(&created.id, "Another description")
            .await
            .unwrap();
        assert_eq!(updated.description, "Another description");
        assert!(
            updated.is_banned,
            "description updates must preserve the derived active room-ban state"
        );
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

    /// Integration test: room member_count counts current member rows.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_with_count_counts_current_members() {
        use crate::models::{RoomMember, RoomRole, User};
        use crate::repository::{RoomMemberRepository, UserRepository};
        use crate::test_helpers::{RoomFixture, UserFixture};

        fn make_user(username: &str) -> User {
            User::new(username.to_string(), crate::models::SignupMethod::Email)
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

        let _ = banned;
        let _ = rejected;

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
            "room member_count should include only current member rows"
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
        assert!(!context.is_in_kick_cooldown);

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
