use sqlx::{PgPool, Row, FromRow};

use crate::{
    models::{Room, RoomId, RoomStatus, RoomSettings, UserId, RoomListQuery, PageParams, MemberStatus},
    Result,
};
use super::query_builder::{WhereClauseBuilder, escape_ilike};

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

impl RoomRepository {
    #[must_use] 
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new room
    pub async fn create(&self, room: &Room) -> Result<Room> {
        let created = sqlx::query_as::<_, Room>(
            "INSERT INTO rooms (id, name, description, created_by, status, is_banned, created_at, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.created_by.as_str())
        .bind(room.status)
        .bind(room.is_banned)
        .bind(room.created_at)
        .bind(room.updated_at)
        .bind(room.version)
        .fetch_one(&self.pool)
        .await?;

        Ok(created)
    }

    /// Create a new room using a provided executor (pool or transaction)
    pub async fn create_with_executor<'e, E>(&self, room: &Room, executor: E) -> Result<Room>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let created = sqlx::query_as::<_, Room>(
            "INSERT INTO rooms (id, name, description, created_by, status, is_banned, created_at, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.created_by.as_str())
        .bind(room.status)
        .bind(room.is_banned)
        .bind(room.created_at)
        .bind(room.updated_at)
        .bind(room.version)
        .fetch_one(executor)
        .await?;

        Ok(created)
    }

    /// Get room by ID
    pub async fn get_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        let room = sqlx::query_as::<_, Room>(
            "SELECT id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
             FROM rooms
             WHERE id = $1 AND deleted_at IS NULL"
        )
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(room)
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
        let updated = sqlx::query_as::<_, Room>(
            "UPDATE rooms
             SET name = $2, description = $3, status = $4, is_banned = $5, version = version + 1
             WHERE id = $1 AND deleted_at IS NULL AND version = $6
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.status)
        .bind(room.is_banned)
        .bind(old_version)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(updated) = updated {
            Ok(updated)
        } else {
            // Check if the room exists at all to distinguish
            // "not found" from "concurrent modification"
            let exists = self.get_by_id(&room.id).await?.is_some();
            if exists {
                Err(crate::Error::OptimisticLockConflict)
            } else {
                Err(crate::Error::NotFound(format!("Room {} not found", room.id.as_str())))
            }
        }
    }

    /// Soft delete room
    pub async fn delete(&self, room_id: &RoomId) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE rooms
             SET deleted_at = $2, updated_at = $2
             WHERE id = $1 AND deleted_at IS NULL"
        )
        .bind(room_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Hard delete room (used for cleanup of partially created rooms).
    ///
    /// This performs a real `DELETE` which triggers `ON DELETE CASCADE` on all
    /// related tables (`room_settings`, `room_members`, playlists, `room_playback_state`,
    /// etc.), ensuring no orphaned rows are left behind.
    pub async fn hard_delete(&self, room_id: &RoomId) -> Result<bool> {
        let result = sqlx::query("DELETE FROM rooms WHERE id = $1")
            .bind(room_id.as_str())
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Build the shared WHERE clause conditions for room list queries.
    fn build_room_list_conditions(query: &RoomListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("r.deleted_at IS NULL");

        match &query.status {
            Some(RoomStatus::Active) => wb.push_literal("r.status = 1"),
            Some(RoomStatus::Pending) => wb.push_literal("r.status = 2"),
            Some(RoomStatus::Closed) => wb.push_literal("r.status = 3"),
            None => {}
        }

        match query.is_banned {
            Some(true) => wb.push_literal("r.is_banned = TRUE"),
            Some(false) => wb.push_literal("r.is_banned = FALSE"),
            None => {}
        }

        if query.search.is_some() {
            wb.push_param("(r.name ILIKE ${idx} OR r.description ILIKE ${idx})");
        }

        wb
    }

    /// Bind the search pattern onto a `query_scalar` if present.
    fn bind_search_scalar<'q>(
        qb: sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments>,
        search_pattern: &'q Option<String>,
    ) -> sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments> {
        match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        }
    }

    /// Bind the search pattern onto a `query_as` if present.
    fn bind_search<'q, O>(
        qb: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
        search_pattern: &'q Option<String>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        }
    }

    /// List rooms with pagination and filters
    pub async fn list(&self, query: &RoomListQuery) -> Result<(Vec<Room>, i64)> {
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let wb = Self::build_room_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!("SELECT COUNT(*) as count FROM rooms r WHERE {count_where}");
        let count: i64 = Self::bind_search_scalar(sqlx::query_scalar(&count_sql), &search_pattern)
            .fetch_one(&self.pool)
            .await?;

        // List query: $1=limit, $2=offset, then filter params start at $3
        let (list_where, _) = wb.build(3);
        let list_sql = format!(
            "SELECT r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version
             FROM rooms r
             WHERE {list_where}
             ORDER BY r.created_at DESC
             LIMIT $1 OFFSET $2"
        );
        let list_qb = sqlx::query_as::<_, Room>(&list_sql).bind(limit).bind(offset);
        let rooms: Vec<Room> = Self::bind_search(list_qb, &search_pattern)
            .fetch_all(&self.pool)
            .await?;

        Ok((rooms, count))
    }

    /// List rooms with member count (optimized with JOIN)
    pub async fn list_with_count(&self, query: &RoomListQuery) -> Result<(Vec<crate::models::RoomWithCount>, i64)> {
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let wb = Self::build_room_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!("SELECT COUNT(DISTINCT r.id) FROM rooms r WHERE {count_where}");
        let count: i64 = Self::bind_search_scalar(sqlx::query_scalar(&count_sql), &search_pattern)
            .fetch_one(&self.pool)
            .await?;

        // List query: $1=limit, $2=offset, then filter params start at $3
        let (list_where, _) = wb.build(3);
        let list_sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status, r.is_banned,
                r.created_at, r.updated_at, r.deleted_at, r.version,
                COALESCE(COUNT(rm.user_id) FILTER (WHERE rm.left_at IS NULL), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE {list_where}
            GROUP BY r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version
            ORDER BY r.created_at DESC
            LIMIT $1 OFFSET $2
            "
        );

        let mut list_qb = sqlx::query(&list_sql).bind(limit).bind(offset);
        if let Some(ref pattern) = search_pattern {
            list_qb = list_qb.bind(pattern);
        }
        let rows = list_qb.fetch_all(&self.pool).await?;

        let rooms_with_count: Result<Vec<crate::models::RoomWithCount>> = rows
            .into_iter()
            .map(|row| {
                let member_count: i32 = row.try_get("member_count")?;
                let room = Room::from_row(&row)?;
                Ok(crate::models::RoomWithCount {
                    room,
                    member_count,
                })
            })
            .collect();

        Ok((rooms_with_count?, count))
    }

    /// Check if room exists (not soft-deleted)
    pub async fn exists(&self, room_id: &RoomId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE id = $1 AND deleted_at IS NULL"
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Check if room exists, is active, and is not banned
    ///
    /// This is a stricter check than `exists()` -- it also verifies the room
    /// has status = Active and is not banned, which is the condition for a room
    /// to be joinable/accessible by regular users.
    pub async fn is_accessible(&self, room_id: &RoomId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE id = $1 AND deleted_at IS NULL AND status = 1 AND is_banned = FALSE"
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Get room member count
    pub async fn get_member_count(&self, room_id: &RoomId) -> Result<i32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members
             WHERE room_id = $1 AND left_at IS NULL"
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count as i32)
    }

    /// Get rooms created by a specific user
    pub async fn list_by_creator(&self, creator_id: &UserId, pagination: PageParams) -> Result<(Vec<Room>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Get total count
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL"
        )
        .bind(creator_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get rooms
        let rooms = sqlx::query_as::<_, Room>(
            "SELECT id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL
             ORDER BY created_at DESC
             LIMIT $2 OFFSET $3"
        )
        .bind(creator_id.as_str())
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
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Get total count
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL"
        )
        .bind(creator_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get rooms with member count using LEFT JOIN
        let rows = sqlx::query(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status, r.is_banned,
                r.created_at, r.updated_at, r.deleted_at, r.version,
                COALESCE(COUNT(rm.user_id) FILTER (WHERE rm.left_at IS NULL), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE r.created_by = $1 AND r.deleted_at IS NULL
            GROUP BY r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version
            ORDER BY r.created_at DESC
            LIMIT $2 OFFSET $3
            "
        )
        .bind(creator_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let rooms_with_count: Result<Vec<crate::models::RoomWithCount>> = rows
            .into_iter()
            .map(|row| {
                let member_count: i32 = row.try_get("member_count")?;
                let room = Room::from_row(&row)?;
                Ok(crate::models::RoomWithCount {
                    room,
                    member_count,
                })
            })
            .collect();

        Ok((rooms_with_count?, count))
    }

    /// Update room status
    pub async fn update_status(&self, room_id: &RoomId, status: RoomStatus) -> Result<Room> {
        let room = sqlx::query_as::<_, Room>(
            r"
            UPDATE rooms
            SET status = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
            ",
        )
        .bind(status)
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {} not found", room_id.as_str())))?;

        Ok(room)
    }

    /// Update room ban status (admin only)
    pub async fn update_ban_status(&self, room_id: &RoomId, is_banned: bool) -> Result<Room> {
        let room = sqlx::query_as::<_, Room>(
            r"
            UPDATE rooms
            SET is_banned = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
            ",
        )
        .bind(is_banned)
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {} not found", room_id.as_str())))?;

        Ok(room)
    }

    /// Update room description
    pub async fn update_description(&self, room_id: &RoomId, description: &str) -> Result<Room> {
        let room = sqlx::query_as::<_, Room>(
            r"
            UPDATE rooms
            SET description = $1, updated_at = CURRENT_TIMESTAMP, version = version + 1
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at, version
            ",
        )
        .bind(description)
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {} not found", room_id.as_str())))?;

        Ok(room)
    }

    /// Fetch all data needed by the join-room flow in a single query.
    ///
    /// Combines three lookups that were previously sequential:
    ///   1. `rooms` row (by id, not soft-deleted)
    ///   2. Ban check (`room_members` where status = Banned)
    ///   3. Room settings + password hash (`room_settings`)
    ///
    /// Returns `None` if the room does not exist or is soft-deleted.
    pub async fn get_join_context(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<JoinRoomContext>> {
        let row = sqlx::query(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status, r.is_banned,
                r.created_at, r.updated_at, r.deleted_at, r.version,
                EXISTS(
                    SELECT 1 FROM room_members rm
                    WHERE rm.room_id = r.id AND rm.user_id = $2 AND rm.status = $3
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
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Banned)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };

        let room = Room::from_row(&row)?;
        let is_banned: bool = row.try_get("user_is_banned")?;

        // Deserialize settings from JSON, falling back to defaults
        let settings: RoomSettings = match row.try_get::<Option<String>, _>("settings_json")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| crate::Error::Internal(format!("Failed to deserialize room settings: {e}")))?,
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

    #[test]
    fn test_build_room_list_conditions_no_filters() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
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
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.deleted_at IS NULL"));
        assert!(sql.contains("r.status = 1"));
        assert_eq!(wb.param_count(), 0); // status is a literal, not a param
    }

    #[test]
    fn test_build_room_list_conditions_with_status_pending() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Pending),
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.status = 2"));
    }

    #[test]
    fn test_build_room_list_conditions_with_status_closed() {
        let query = RoomListQuery {
            status: Some(RoomStatus::Closed),
            is_banned: None,
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.status = 3"));
    }

    #[test]
    fn test_build_room_list_conditions_with_banned_filter() {
        let query = RoomListQuery {
            status: None,
            is_banned: Some(true),
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.is_banned = TRUE"));
    }

    #[test]
    fn test_build_room_list_conditions_with_not_banned_filter() {
        let query = RoomListQuery {
            status: None,
            is_banned: Some(false),
            search: None,
            pagination: PageParams::default(),
            creator_id: None,
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.is_banned = FALSE"));
    }

    #[test]
    fn test_build_room_list_conditions_with_search() {
        let query = RoomListQuery {
            status: None,
            is_banned: None,
            search: Some("test".to_string()),
            pagination: PageParams::default(),
            creator_id: None,
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
        };
        let wb = RoomRepository::build_room_list_conditions(&query);

        let (sql, _) = wb.build(1);
        assert!(sql.contains("r.deleted_at IS NULL"));
        assert!(sql.contains("r.status = 1"));
        assert!(sql.contains("r.is_banned = FALSE"));
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

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_room() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner user first (rooms have FK to users)
        let owner = UserFixture::new().with_username("room_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Test Room")
            .with_description("desc")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert_eq!(created.name, "Test Room");
        assert_eq!(created.created_by, owner.id);
        assert!(room_repo.exists(&created.id).await.unwrap());
    }

    /// Unit test: Room model methods
    #[test]
    fn test_room_model_new() {
        let creator_id = UserId::new();
        let room = Room::new("My Room".to_string(), creator_id.clone());

        assert_eq!(room.name, "My Room");
        assert!(room.description.is_empty());
        assert_eq!(room.created_by, creator_id);
        assert_eq!(room.status, RoomStatus::Active);
        assert!(!room.is_banned);
        assert!(room.deleted_at.is_none());
        assert!(room.is_active());
    }

    /// Unit test: Room::new_with_description
    #[test]
    fn test_room_model_new_with_description() {
        let creator_id = UserId::new();
        let room = Room::new_with_description(
            "My Room".to_string(),
            "A test room".to_string(),
            creator_id.clone(),
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
        assert!(!room.is_active()); // Banned rooms are not active

        room.unban();
        assert!(!room.is_banned());
        assert!(room.is_active()); // Active again after unban
    }

    /// Unit test: RoomStatus enum
    #[test]
    fn test_room_status() {
        assert_eq!(RoomStatus::Active.as_str(), "active");
        assert_eq!(RoomStatus::Pending.as_str(), "pending");
        assert_eq!(RoomStatus::Closed.as_str(), "closed");

        assert!(RoomStatus::Active.is_active());
        assert!(RoomStatus::Pending.is_pending());
        assert!(RoomStatus::Closed.is_closed());
    }

    /// Unit test: Room::is_active() with various states
    #[test]
    fn test_room_is_active_combinations() {
        let creator_id = UserId::new();

        // Active status, not banned, not deleted
        let mut room = Room::new("Test".to_string(), creator_id.clone());
        assert!(room.is_active());

        // Banned
        room.is_banned = true;
        assert!(!room.is_active());

        // Not banned but closed status
        room.is_banned = false;
        room.status = RoomStatus::Closed;
        assert!(!room.is_active());

        // Deleted
        room.status = RoomStatus::Active;
        room.deleted_at = Some(chrono::Utc::now());
        assert!(!room.is_active());
    }

    /// Integration test: Get non-existent room returns None
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_nonexistent_room() {
        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let room_repo = RoomRepository::new(infra.pool.clone());

        let room_id = RoomId::from_string("nonexistent".to_string());
        let result = room_repo.get_by_id(&room_id).await.unwrap();
        assert!(result.is_none());
    }

    /// Integration test: Update room
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_room() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("update_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Original Name")
            .with_description("Original description")
            .with_owner(owner.id.clone())
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
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("delete_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room to Delete")
            .with_owner(owner.id.clone())
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
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("hard_delete_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Room to Hard Delete")
            .with_owner(owner.id.clone())
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
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("status_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Status Test Room")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert_eq!(created.status, RoomStatus::Active);

        // Update to Pending
        let updated = room_repo.update_status(&created.id, RoomStatus::Pending).await.unwrap();
        assert_eq!(updated.status, RoomStatus::Pending);

        // Update to Closed
        let updated = room_repo.update_status(&created.id, RoomStatus::Closed).await.unwrap();
        assert_eq!(updated.status, RoomStatus::Closed);
    }

    /// Integration test: Update room ban status
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_ban_status() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("ban_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Ban Test Room")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();
        assert!(!created.is_banned);

        // Ban room
        let updated = room_repo.update_ban_status(&created.id, true).await.unwrap();
        assert!(updated.is_banned);

        // Unban room
        let updated = room_repo.update_ban_status(&created.id, false).await.unwrap();
        assert!(!updated.is_banned);
    }

    /// Integration test: Update room description
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_description() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("desc_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Desc Test Room")
            .with_description("Original description")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Update description
        let updated = room_repo.update_description(&created.id, "New description").await.unwrap();
        assert_eq!(updated.description, "New description");
    }

    /// Integration test: List rooms with pagination
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_rooms_pagination() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("list_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create 15 rooms
        for i in 0..15 {
            let room = RoomFixture::new()
                .with_name(&format!("List Room {}", i))
                .with_owner(owner.id.clone())
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
        };
        let (rooms, total) = room_repo.list(&query).await.unwrap();
        assert_eq!(rooms.len(), 5);
        assert_eq!(total, 15);
    }

    /// Integration test: List rooms with filters
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_rooms_with_filters() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("filter_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create active room
        let room = RoomFixture::new()
            .with_name("Active Room")
            .with_owner(owner.id.clone())
            .build();
        room_repo.create(&room).await.unwrap();

        // Create pending room
        let mut pending_room = RoomFixture::new()
            .with_name("Pending Room")
            .with_owner(owner.id.clone())
            .build();
        pending_room.status = RoomStatus::Pending;
        room_repo.create(&pending_room).await.unwrap();

        // Create and ban a room
        let mut banned_room = RoomFixture::new()
            .with_name("Banned Room")
            .with_owner(owner.id.clone())
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
        };
        let (rooms, _) = room_repo.list(&query).await.unwrap();
        assert!(rooms.iter().all(|r| r.name.contains("Active")));
    }

    /// Integration test: List rooms by creator
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_list_by_creator() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create two users
        let owner1 = UserFixture::new().with_username("creator1").build();
        let owner1 = user_repo.create(&owner1).await.unwrap();

        let owner2 = UserFixture::new().with_username("creator2").build();
        let owner2 = user_repo.create(&owner2).await.unwrap();

        // Create rooms for owner1
        for i in 0..3 {
            let room = RoomFixture::new()
                .with_name(&format!("Owner1 Room {}", i))
                .with_owner(owner1.id.clone())
                .build();
            room_repo.create(&room).await.unwrap();
        }

        // Create rooms for owner2
        for i in 0..2 {
            let room = RoomFixture::new()
                .with_name(&format!("Owner2 Room {}", i))
                .with_owner(owner2.id.clone())
                .build();
            room_repo.create(&room).await.unwrap();
        }

        // List by creator
        let (rooms, total) = room_repo.list_by_creator(&owner1.id, PageParams::default()).await.unwrap();
        assert_eq!(rooms.len(), 3);
        assert_eq!(total, 3);

        let (rooms, total) = room_repo.list_by_creator(&owner2.id, PageParams::default()).await.unwrap();
        assert_eq!(rooms.len(), 2);
        assert_eq!(total, 2);
    }

    /// Integration test: is_accessible
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_is_accessible() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("accessible_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create active room
        let room = RoomFixture::new()
            .with_name("Accessible Room")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Active room is accessible
        assert!(room_repo.is_accessible(&created.id).await.unwrap());

        // Ban room
        room_repo.update_ban_status(&created.id, true).await.unwrap();
        assert!(!room_repo.is_accessible(&created.id).await.unwrap());

        // Unban and close
        room_repo.update_ban_status(&created.id, false).await.unwrap();
        room_repo.update_status(&created.id, RoomStatus::Closed).await.unwrap();
        assert!(!room_repo.is_accessible(&created.id).await.unwrap());
    }

    /// Integration test: get_join_context
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_join_context() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("join_context_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Join Context Room")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();

        // Get join context
        let context = room_repo.get_join_context(&created.id, &owner.id).await.unwrap();
        assert!(context.is_some());

        let context = context.unwrap();
        assert_eq!(context.room.id, created.id);
        assert!(!context.is_banned); // Owner is not banned

        // Non-existent room returns None
        let non_existent = RoomId::from_string("nonexistent".to_string());
        let context = room_repo.get_join_context(&non_existent, &owner.id).await.unwrap();
        assert!(context.is_none());
    }

    /// Integration test: create_with_executor
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_with_executor() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner
        let owner = UserFixture::new().with_username("executor_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create room with executor (pool)
        let room = RoomFixture::new()
            .with_name("Executor Room")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create_with_executor(&room, &infra.pool).await.unwrap();
        assert_eq!(created.name, "Executor Room");
    }

    /// Integration test: Room not found error handling
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_nonexistent_room() {
        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Try to update non-existent room
        let room = Room::new("Non-existent".to_string(), UserId::new());
        let result = room_repo.update(&room, 0).await;
        assert!(matches!(result, Err(crate::Error::NotFound(_))));
    }

    /// Integration test: Optimistic lock conflict
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_stale_version_returns_optimistic_lock_conflict() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("optimistic_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Optimistic Room")
            .with_description("original")
            .with_owner(owner.id.clone())
            .build();
        let created = room_repo.create(&room).await.unwrap();
        let original_version = created.version;

        // First update succeeds
        let mut updated_room = created.clone();
        updated_room.name = "Updated Name V1".to_string();
        updated_room.description = "updated v1".to_string();
        let v1 = room_repo.update(&updated_room, original_version).await.unwrap();
        assert_eq!(v1.version, original_version + 1);
        assert_eq!(v1.name, "Updated Name V1");

        // Second update with stale version (original_version) -> should get OptimisticLockConflict
        let mut stale_room = created.clone();
        stale_room.name = "Updated Name V2".to_string();
        stale_room.description = "updated v2".to_string();
        let err = room_repo.update(&stale_room, original_version).await.unwrap_err();
        assert!(
            matches!(err, crate::Error::OptimisticLockConflict),
            "Expected OptimisticLockConflict, got: {:?}", err
        );
    }

    /// Integration test: Update soft-deleted room returns NotFound
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_soft_deleted_room_returns_not_found() {
        use crate::test_helpers::{UserFixture, RoomFixture};
        use crate::repository::user::UserRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("softdel_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Soft Delete Room")
            .with_owner(owner.id.clone())
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
            "Expected NotFound for soft-deleted room, got: {:?}", err
        );
    }
}
