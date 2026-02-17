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
            "INSERT INTO rooms (id, name, description, created_by, status, is_banned, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.created_by.as_str())
        .bind(room.status)
        .bind(room.is_banned)
        .bind(room.created_at)
        .bind(room.updated_at)
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
            "INSERT INTO rooms (id, name, description, created_by, status, is_banned, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.created_by.as_str())
        .bind(room.status)
        .bind(room.is_banned)
        .bind(room.created_at)
        .bind(room.updated_at)
        .fetch_one(executor)
        .await?;

        Ok(created)
    }

    /// Get room by ID
    pub async fn get_by_id(&self, room_id: &RoomId) -> Result<Option<Room>> {
        let room = sqlx::query_as::<_, Room>(
            "SELECT id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at
             FROM rooms
             WHERE id = $1 AND deleted_at IS NULL"
        )
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(room)
    }

    /// Update room
    pub async fn update(&self, room: &Room) -> Result<Room> {
        let updated = sqlx::query_as::<_, Room>(
            "UPDATE rooms
             SET name = $2, description = $3, status = $4, is_banned = $5, updated_at = $6
             WHERE id = $1 AND deleted_at IS NULL
             RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at"
        )
        .bind(room.id.as_str())
        .bind(&room.name)
        .bind(&room.description)
        .bind(room.status)
        .bind(room.is_banned)
        .bind(chrono::Utc::now())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| crate::Error::NotFound(format!("Room {} not found", room.id.as_str())))?;

        Ok(updated)
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
            "SELECT r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at
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
                r.created_at, r.updated_at, r.deleted_at,
                COALESCE(COUNT(rm.user_id) FILTER (WHERE rm.left_at IS NULL), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE {list_where}
            GROUP BY r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at
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
            "SELECT id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at
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
                r.created_at, r.updated_at, r.deleted_at,
                COALESCE(COUNT(rm.user_id) FILTER (WHERE rm.left_at IS NULL), 0)::int as member_count
            FROM rooms r
            LEFT JOIN room_members rm ON r.id = rm.room_id
            WHERE r.created_by = $1 AND r.deleted_at IS NULL
            GROUP BY r.id, r.name, r.description, r.created_by, r.status, r.is_banned, r.created_at, r.updated_at, r.deleted_at
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
            SET status = $1, updated_at = CURRENT_TIMESTAMP
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at
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
            SET is_banned = $1, updated_at = CURRENT_TIMESTAMP
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at
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
            SET description = $1, updated_at = CURRENT_TIMESTAMP
            WHERE id = $2 AND deleted_at IS NULL
            RETURNING id, name, description, created_by, status, is_banned, created_at, updated_at, deleted_at
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
                r.created_at, r.updated_at, r.deleted_at,
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
    #[ignore = "Requires Docker (PostgreSQL/Redis)"]
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
}
