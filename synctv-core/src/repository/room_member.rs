use sqlx::{postgres::PgRow, PgPool, Row};

use crate::{
    models::{
        MemberStatus, MyRoomListQuery, MyRoomListSortBy, MyRoomRelation, PageParams, RoomId,
        RoomMember, RoomMemberListQuery, RoomMemberListSortBy, RoomMemberWithUser, RoomRole,
        RoomStatus, UserId,
    },
    service::AddMemberOptions,
    Error, Result,
};

use super::query_builder::{escape_ilike, WhereClauseBuilder};

/// Room member repository for database operations
#[derive(Clone)]
pub struct RoomMemberRepository {
    pool: PgPool,
}

struct RoomMemberWithUserRow {
    room_id: RoomId,
    user_id: UserId,
    username: String,
    role: RoomRole,
    added_permissions: i64,
    removed_permissions: i64,
    admin_added_permissions: i64,
    admin_removed_permissions: i64,
    joined_at: chrono::DateTime<chrono::Utc>,
    left_at: Option<chrono::DateTime<chrono::Utc>>,
    is_banned: bool,
    is_active: bool,
    banned_at: Option<chrono::DateTime<chrono::Utc>>,
    banned_reason: Option<String>,
}

const ACCESSIBLE_ROOM_CREATOR_CONDITION: &str =
    "EXISTS (SELECT 1 FROM users u WHERE u.id = r.created_by AND u.deleted_at IS NULL
        AND NOT EXISTS (
            SELECT 1 FROM user_bans ub
            WHERE ub.user_id = u.id
              AND ub.revoked_at IS NULL
              AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ))";
const ROOM_MEMBER_RETURNING_COLUMNS: &str = "room_id, user_id, role,
    added_permissions, removed_permissions, admin_added_permissions, admin_removed_permissions,
    joined_at, left_at, version";
const ROOM_MEMBER_SELECT_COLUMNS: &str = "rm.room_id, rm.user_id, rm.role,
    rm.added_permissions, rm.removed_permissions, rm.admin_added_permissions, rm.admin_removed_permissions,
    rm.joined_at, rm.left_at, rm.version,
    (
        SELECT rmb.starts_at FROM room_member_bans rmb
        WHERE rmb.room_id = rm.room_id
          AND rmb.user_id = rm.user_id
          AND rmb.revoked_at IS NULL
          AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
        ORDER BY rmb.starts_at DESC
        LIMIT 1
    ) AS banned_at,
    (
        SELECT rmb.banned_by FROM room_member_bans rmb
        WHERE rmb.room_id = rm.room_id
          AND rmb.user_id = rm.user_id
          AND rmb.revoked_at IS NULL
          AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
        ORDER BY rmb.starts_at DESC
        LIMIT 1
    ) AS banned_by,
    (
        SELECT rmb.reason FROM room_member_bans rmb
        WHERE rmb.room_id = rm.room_id
          AND rmb.user_id = rm.user_id
          AND rmb.revoked_at IS NULL
          AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
        ORDER BY rmb.starts_at DESC
        LIMIT 1
    ) AS banned_reason";
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
const ACTIVE_MEMBER_BAN_EXISTS_SQL: &str = "EXISTS (
    SELECT 1 FROM room_member_bans rmb
    WHERE rmb.room_id = rm.room_id
      AND rmb.user_id = rm.user_id
      AND rmb.revoked_at IS NULL
      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
)";
const ACTIVE_MEMBER_BAN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM room_member_bans rmb
    WHERE rmb.room_id = rm.room_id
      AND rmb.user_id = rm.user_id
      AND rmb.revoked_at IS NULL
      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
)";

fn permission_bits_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        Error::InvalidInput(format!("permission bits value {value} exceeds i64 range"))
    })
}

fn db_permission_i64_to_u64(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        Error::Internal(format!(
            "database returned negative permission bits for column {column}"
        ))
    })
}

fn pagination_u64_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn count_i64_to_u64_saturating(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl RoomMemberRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn build_room_member_order_by(query: &RoomMemberListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            RoomMemberListSortBy::JoinedAt => {
                format!("rm.joined_at {direction}, rm.user_id ASC")
            }
            RoomMemberListSortBy::Username => format!("u.username {direction}, rm.user_id ASC"),
            RoomMemberListSortBy::Role => {
                format!("rm.role {direction}, rm.joined_at ASC, rm.user_id ASC")
            }
            RoomMemberListSortBy::Status => {
                format!("rm.left_at {direction} NULLS FIRST, rm.joined_at ASC, rm.user_id ASC")
            }
        }
    }

    fn build_my_room_list_conditions(query: &MyRoomListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("rm.left_at IS NULL");
        wb.push_literal("r.deleted_at IS NULL");

        match query.status {
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
            wb.push_param(
                "(r.name ILIKE ${idx} ESCAPE '\\' OR r.description ILIKE ${idx} ESCAPE '\\')",
            );
        }

        match query.relation {
            MyRoomRelation::All => {}
            MyRoomRelation::Created => wb.push_literal("r.created_by = $1"),
            MyRoomRelation::Participating => wb.push_literal("r.created_by != $1"),
        }

        wb
    }

    fn bind_my_room_filters<'q>(
        qb: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
        search_pattern: Option<&'q str>,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        }
    }

    fn build_my_room_order_by(query: &MyRoomListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            MyRoomListSortBy::JoinedAt => {
                format!("rm.joined_at {direction}, r.id {direction}")
            }
            MyRoomListSortBy::Name => format!("r.name {direction}, r.id {direction}"),
            MyRoomListSortBy::CreatedAt => {
                format!("r.created_at {direction}, r.id {direction}")
            }
            MyRoomListSortBy::UpdatedAt => {
                format!("r.updated_at {direction}, r.id {direction}")
            }
            MyRoomListSortBy::LastActivityAt => {
                format!("r.last_activity_at {direction} NULLS LAST, r.id {direction}")
            }
        }
    }

    /// Add user to room with role.
    ///
    /// # Re-join semantics
    ///
    /// This method intentionally allows users who previously left a room to
    /// rejoin freely. When an `ON CONFLICT` row exists **and the user is not
    /// banned**, the `DO UPDATE` branch resets `left_at` to `NULL`, refreshes
    /// `joined_at`, and bumps the version. This is the designed rejoin flow:
    /// left users can re-enter without needing an explicit invite or approval.
    ///
    /// When the `ON CONFLICT` row exists but the `DO UPDATE... WHERE` condition
    /// is not satisfied (user is **banned**), no row is returned. In that case a
    /// follow-up query determines the specific reason and returns a semantic
    /// error (`Authorization` for banned).
    pub async fn add(&self, member: &RoomMember) -> Result<RoomMember> {
        let sql = format!(
            "INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                admin_added_permissions = 0,
                admin_removed_permissions = 0,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE NOT EXISTS (
                SELECT 1 FROM room_member_bans rmb
                WHERE rmb.room_id = room_members.room_id
                  AND rmb.user_id = room_members.user_id
                  AND rmb.revoked_at IS NULL
                  AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
             )
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let result = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(member.room_id)
            .bind(member.user_id)
            .bind(member.role)
            .bind(permission_bits_to_i64(member.added_permissions)?)
            .bind(permission_bits_to_i64(member.removed_permissions)?)
            .bind(member.joined_at)
            .bind(member.version)
            .fetch_optional(&self.pool)
            .await?;

        match result {
            Some(m) => Ok(m),
            None => {
                // The ON CONFLICT WHERE condition was not met. Determine why.
                self.diagnose_add_conflict(&member.room_id, &member.user_id, &self.pool)
                    .await
            }
        }
    }

    /// Add user to room using a provided connection (pool or transaction)
    ///
    /// Accepts `&mut PgConnection` so the connection can be reborrowed for
    /// the fallback `diagnose_add_conflict` query (fixes reading outside
    /// the caller's transaction).
    ///
    /// See [`add`] for the `ON CONFLICT` semantics and error handling.
    pub async fn add_with_executor(
        &self,
        member: &RoomMember,
        conn: &mut sqlx::PgConnection,
    ) -> Result<RoomMember> {
        let sql = format!(
            "INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                admin_added_permissions = 0,
                admin_removed_permissions = 0,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE NOT EXISTS (
                SELECT 1 FROM room_member_bans rmb
                WHERE rmb.room_id = room_members.room_id
                  AND rmb.user_id = room_members.user_id
                  AND rmb.revoked_at IS NULL
                  AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
             )
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let result = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(member.room_id)
            .bind(member.user_id)
            .bind(member.role)
            .bind(permission_bits_to_i64(member.added_permissions)?)
            .bind(permission_bits_to_i64(member.removed_permissions)?)
            .bind(member.joined_at)
            .bind(member.version)
            .fetch_optional(&mut *conn)
            .await?;

        match result {
            Some(m) => Ok(m),
            None => {
                // The ON CONFLICT WHERE condition was not met. Determine why.
                self.diagnose_add_conflict(&member.room_id, &member.user_id, &mut *conn)
                    .await
            }
        }
    }

    /// Add user to room with role and options in a single transaction
    ///
    /// This method performs all checks and the insert operation in a single database transaction:
    /// - Check if room exists and is active
    /// - Check if user is already a member
    /// - Check max members limit
    /// - Insert the new member
    ///
    /// All checks use SELECT... FOR UPDATE to lock rows and prevent race conditions.
    ///
    /// # Arguments
    ///
    /// * `member` - The member to add
    /// * `options` - Options controlling which checks to perform and limits to enforce
    pub async fn add_with_options(
        &self,
        member: &RoomMember,
        options: &AddMemberOptions,
    ) -> Result<RoomMember> {
        let mut tx = self.pool.begin().await?;
        let result = self.add_with_options_tx(member, options, &mut tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Add user to room with role and options using a provided transaction
    ///
    /// Same as `add_with_options` but lets the caller control the transaction boundary.
    pub async fn add_with_options_tx(
        &self,
        member: &RoomMember,
        options: &AddMemberOptions,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<RoomMember> {
        // 1. Check if room exists and lock the row
        let room_row = sqlx::query!(
            "SELECT id, closed_at FROM rooms
             WHERE id = $1
             FOR UPDATE",
            member.room_id as RoomId,
        )
        .fetch_optional(&mut **tx)
        .await?;

        let Some(room_row) = room_row else {
            return Err(Error::NotFound("Room not found".to_string()));
        };

        // 2. Check if room is active (if option enabled)
        if options.check_room_active && room_row.closed_at.is_some() {
            return Err(Error::InvalidInput("Room is not active".to_string()));
        }

        // 3. Check if user is already a member (if option enabled)
        if options.check_duplicate {
            let existing = sqlx::query_scalar!(
                "SELECT user_id FROM room_members
                 WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
                 FOR UPDATE",
                member.room_id as RoomId,
                member.user_id as UserId,
            )
            .fetch_optional(&mut **tx)
            .await?;

            if existing.is_some() {
                return Err(Error::AlreadyExists(
                    "Already a member of this room".to_string(),
                ));
            }
        }

        // 4. Check max members limit (if option enabled)
        // When max_members is 0 or None, treat as unlimited (no enforcement).
        // IMPORTANT: We use a subquery with FOR UPDATE to lock all
        // member rows, then count them. This prevents TOCTOU races where two
        // concurrent transactions could both see count < max and both insert,
        // exceeding the limit. PostgreSQL doesn't allow FOR UPDATE directly
        // with aggregate functions like COUNT(*).
        if options.check_max_members {
            let max_members = options.max_members;
            if max_members > 0 {
                sqlx::query!(
                    "SELECT id FROM rooms WHERE id = $1 FOR UPDATE",
                    member.room_id as RoomId,
                )
                .fetch_optional(&mut **tx)
                .await?;

                // Lock all member rows for this room to prevent concurrent inserts
                // from seeing the same count value
                let count = sqlx::query_scalar!(
                    "SELECT COUNT(*) as count FROM (
                        SELECT 1 FROM room_members
                        WHERE room_id = $1 AND left_at IS NULL
                        FOR UPDATE
                    ) sub",
                    member.room_id as RoomId,
                )
                .fetch_one(&mut **tx)
                .await?
                .unwrap_or(0);
                let current_count = count_i64_to_u64_saturating(count);
                if current_count >= max_members {
                    tracing::warn!(
                        room_id = %member.room_id,
                        max_members = max_members,
                        current_count = count,
                        "Room is full, rejecting join"
                    );
                    return Err(Error::InvalidInput(format!(
                        "Room is full ({count}/{max_members} members)"
                    )));
                }
            }
            // max_members == 0 means unlimited — no check needed
        }

        // 5. Insert the new member
        let sql = format!(
            "INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                admin_added_permissions = 0,
                admin_removed_permissions = 0,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE NOT EXISTS (
                SELECT 1 FROM room_member_bans rmb
                WHERE rmb.room_id = room_members.room_id
                  AND rmb.user_id = room_members.user_id
                  AND rmb.revoked_at IS NULL
                  AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
             )
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let result = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(member.room_id)
            .bind(member.user_id)
            .bind(member.role)
            .bind(permission_bits_to_i64(member.added_permissions)?)
            .bind(permission_bits_to_i64(member.removed_permissions)?)
            .bind(member.joined_at)
            .bind(member.version)
            .fetch_optional(&mut **tx)
            .await?;

        match result {
            Some(m) => Ok(m),
            None => {
                // The ON CONFLICT WHERE condition was not met. Determine why.
                self.diagnose_add_conflict(&member.room_id, &member.user_id, &mut **tx)
                    .await
            }
        }
    }

    /// Remove a user from all rooms (soft delete - set `status = Left` and `left_at`).
    ///
    /// Used during user deletion/ban to clean up room memberships.
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        self.remove_all_for_user_with_executor(user_id, &self.pool)
            .await
    }

    /// Remove a user from all rooms using a provided executor (pool or transaction).
    ///
    /// Used to keep user lifecycle mutations and membership cleanup in the same
    /// database transaction.
    pub async fn remove_all_for_user_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<u64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            "UPDATE room_members
             SET left_at = $2, version = version + 1
             WHERE user_id = $1 AND left_at IS NULL",
            user_id as &UserId,
            chrono::Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Remove all active members from the provided rooms.
    ///
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_rooms_with_executor<'e, E>(
        &self,
        room_ids: &[RoomId],
        executor: E,
    ) -> Result<u64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if room_ids.is_empty() {
            return Ok(0);
        }

        let room_id_strs: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();
        let result = sqlx::query!(
            "UPDATE room_members
             SET left_at = $2, version = version + 1
             WHERE room_id = ANY($1) AND left_at IS NULL",
            &room_id_strs,
            chrono::Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Remove user from room (soft delete - set `status = Left` and `left_at`)
    pub async fn remove(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        self.remove_with_executor(room_id, user_id, &self.pool)
            .await
    }

    pub async fn remove_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            "UPDATE room_members
             SET left_at = $3, version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL",
            room_id as &RoomId,
            user_id as &UserId,
            chrono::Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get member by room and user
    pub async fn get(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let sql = format!(
            "SELECT {ROOM_MEMBER_SELECT_COLUMNS}
             FROM room_members rm
             WHERE rm.room_id = $1 AND rm.user_id = $2 AND rm.left_at IS NULL
               AND {ACTIVE_MEMBER_BAN_NOT_EXISTS_SQL}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(member)
    }

    /// Get member by ID (including banned/inactive)
    pub async fn get_any(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        self.get_any_with_executor(room_id, user_id, &self.pool)
            .await
    }

    pub async fn get_any_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<Option<RoomMember>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            "SELECT {ROOM_MEMBER_SELECT_COLUMNS}
             FROM room_members rm
             WHERE rm.room_id = $1 AND rm.user_id = $2"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .fetch_optional(executor)
            .await?;

        Ok(member)
    }

    /// List all active members in a room
    pub async fn list_by_room(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query_as!(
            RoomMemberWithUserRow,
            r#"SELECT
                rm.room_id AS "room_id: RoomId",
                rm.user_id AS "user_id: UserId",
                rm.role AS "role: RoomRole",
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.left_at,
                FALSE AS "is_banned!",
                TRUE AS "is_active!",
                NULL::timestamptz AS "banned_at?",
                NULL::text AS banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND rm.left_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM room_member_bans rmb
                   WHERE rmb.room_id = rm.room_id
                     AND rmb.user_id = rm.user_id
                     AND rmb.revoked_at IS NULL
                     AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
               )
               AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC"#,
            room_id.as_i64()
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(Self::typed_row_to_member_with_user)
            .collect()
    }

    /// List active members in a room with database-level pagination
    ///
    /// Uses `COUNT(*) OVER()` window function to atomically get total count and data
    /// in a single query, avoiding the race condition of separate COUNT + SELECT queries.
    ///
    /// Returns a tuple of (members, total_count) where:
    /// - `members`: Vec of `RoomMemberWithUser` for the current page
    /// - `total_count`: Total number of active members in the room (i64)
    ///
    /// # Exclusions
    ///
    /// This method excludes:
    /// - Members who have left the room (`left_at IS NOT NULL`)
    /// - Banned members (`banned_at IS NOT NULL`)
    /// - Soft-deleted users (`u.deleted_at IS NOT NULL`)
    pub async fn list_by_room_paginated(
        &self,
        room_id: &RoomId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        self.list_by_room_query(
            room_id,
            &RoomMemberListQuery {
                pagination,
                ..Default::default()
            },
        )
        .await
    }

    pub async fn list_by_room_query(
        &self,
        room_id: &RoomId,
        query: &RoomMemberListQuery,
    ) -> Result<(Vec<RoomMemberWithUser>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));

        let mut count_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT COUNT(*) FROM room_members rm JOIN users u ON rm.user_id = u.id WHERE rm.room_id = ",
        );
        count_builder.push_bind(room_id);
        match query.status {
            Some(MemberStatus::Left) => {
                count_builder.push(" AND rm.left_at IS NOT NULL");
            }
            Some(MemberStatus::Active) | None => {
                count_builder.push(" AND rm.left_at IS NULL");
            }
        }
        count_builder.push(" AND ");
        if let Some(true) = query.is_banned {
            count_builder.push(ACTIVE_MEMBER_BAN_EXISTS_SQL);
        } else {
            count_builder.push(ACTIVE_MEMBER_BAN_NOT_EXISTS_SQL);
        }
        count_builder.push(" AND u.deleted_at IS NULL");
        if let Some(pattern) = &search_pattern {
            count_builder.push(" AND (u.username ILIKE ");
            count_builder.push_bind(pattern);
            count_builder.push(" OR rm.user_id::text ILIKE ");
            count_builder.push_bind(pattern);
            count_builder.push(")");
        }
        if let Some(role) = &query.role {
            count_builder.push(" AND rm.role = ");
            count_builder.push_bind(role);
        }
        let total_count: i64 = count_builder
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await?;

        let order_by = Self::build_room_member_order_by(query);
        let mut list_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT
                rm.room_id, rm.user_id, rm.role,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.left_at,
                EXISTS (
                    SELECT 1 FROM room_member_bans rmb
                    WHERE rmb.room_id = rm.room_id
                      AND rmb.user_id = rm.user_id
                      AND rmb.revoked_at IS NULL
                      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                ) AS is_banned,
                (
                    SELECT rmb.reason FROM room_member_bans rmb
                    WHERE rmb.room_id = rm.room_id
                      AND rmb.user_id = rm.user_id
                      AND rmb.revoked_at IS NULL
                      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                    ORDER BY rmb.starts_at DESC
                    LIMIT 1
                ) AS banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = ",
        );
        list_builder.push_bind(room_id);
        match query.status {
            Some(MemberStatus::Left) => {
                list_builder.push(" AND rm.left_at IS NOT NULL");
            }
            Some(MemberStatus::Active) | None => {
                list_builder.push(" AND rm.left_at IS NULL");
            }
        }
        list_builder.push(" AND ");
        if let Some(true) = query.is_banned {
            list_builder.push(ACTIVE_MEMBER_BAN_EXISTS_SQL);
        } else {
            list_builder.push(ACTIVE_MEMBER_BAN_NOT_EXISTS_SQL);
        }
        list_builder.push(" AND u.deleted_at IS NULL");
        if let Some(pattern) = &search_pattern {
            list_builder.push(" AND (u.username ILIKE ");
            list_builder.push_bind(pattern);
            list_builder.push(" OR rm.user_id::text ILIKE ");
            list_builder.push_bind(pattern);
            list_builder.push(")");
        }
        if let Some(role) = &query.role {
            list_builder.push(" AND rm.role = ");
            list_builder.push_bind(role);
        }
        list_builder.push(format!(" ORDER BY {order_by} LIMIT "));
        list_builder.push_bind(limit);
        list_builder.push(" OFFSET ");
        list_builder.push_bind(offset);

        let rows = list_builder.build().fetch_all(&self.pool).await?;
        let members: Result<Vec<RoomMemberWithUser>> = rows
            .into_iter()
            .map(|row| Self::row_to_member_with_user(&row))
            .collect();

        Ok((members?, total_count))
    }

    /// List all active members in a room with online status
    pub async fn list_by_room_with_online(
        &self,
        room_id: &RoomId,
        online_user_ids: &[UserId],
    ) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query_as!(
            RoomMemberWithUserRow,
            r#"SELECT
                rm.room_id AS "room_id: RoomId",
                rm.user_id AS "user_id: UserId",
                rm.role AS "role: RoomRole",
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.left_at,
                FALSE AS "is_banned!",
                TRUE AS "is_active!",
                NULL::timestamptz AS "banned_at?",
                NULL::text AS banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND rm.left_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM room_member_bans rmb
                   WHERE rmb.room_id = rm.room_id
                     AND rmb.user_id = rm.user_id
                     AND rmb.revoked_at IS NULL
                     AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
               )
               AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC"#,
            room_id.as_i64()
        )
        .fetch_all(&self.pool)
        .await?;

        let online_set: std::collections::HashSet<_> = online_user_ids
            .iter()
            .map(super::super::models::id::UserId::as_i64)
            .collect();

        rows.into_iter()
            .map(|row| {
                let mut member = Self::typed_row_to_member_with_user(row)?;
                member.is_online = online_set.contains(&member.user_id.as_i64());
                Ok(member)
            })
            .collect()
    }

    /// Update member role with optimistic locking
    ///
    /// Only updates members that are still active (`left_at IS NULL`). Members
    /// who have left the room will not have their role modified; the call returns
    /// `OptimisticLockConflict` in that case (same as a version mismatch).
    pub async fn update_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                role = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(role)
            .bind(current_version)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member role inside an existing transaction with optimistic locking.
    pub async fn update_role_with_version_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
        executor: impl sqlx::PgExecutor<'_>,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                role = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(role)
            .bind(current_version)
            .fetch_optional(executor)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member status with optimistic locking
    ///
    /// Only updates members that are still active (`left_at IS NULL`). Members
    /// who have left the room will not have their status modified; the call
    /// returns `OptimisticLockConflict` in that case (same as a version mismatch).
    pub async fn update_status(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        status: MemberStatus,
        current_version: i64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                left_at = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let left_at = match status {
            MemberStatus::Active => None,
            MemberStatus::Left => Some(chrono::Utc::now()),
        };
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(left_at)
            .bind(current_version)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member Allow/Deny permissions with optimistic locking
    ///
    /// Only updates members that are still active (`left_at IS NULL`). Members
    /// who have left the room will not have their permissions modified; the call
    /// returns `OptimisticLockConflict` in that case (same as a version mismatch).
    pub async fn update_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
    ) -> Result<RoomMember> {
        self.update_permissions_with_executor(
            room_id,
            user_id,
            added_permissions,
            removed_permissions,
            current_version,
            &self.pool,
        )
        .await
    }

    pub async fn update_permissions_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            "UPDATE room_members
             SET
                added_permissions = $3,
                removed_permissions = $4,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(added_permissions)?)
            .bind(permission_bits_to_i64(removed_permissions)?)
            .bind(current_version)
            .fetch_optional(executor)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update admin-specific Allow/Deny permissions with optimistic locking.
    ///
    /// Only updates members that are still active (`left_at IS NULL`). Members
    /// who have left the room will not have their permissions modified; the call
    /// returns `OptimisticLockConflict` in that case (same as a version mismatch).
    pub async fn update_admin_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
    ) -> Result<RoomMember> {
        self.update_admin_permissions_with_executor(
            room_id,
            user_id,
            added_permissions,
            removed_permissions,
            current_version,
            &self.pool,
        )
        .await
    }

    pub async fn update_admin_permissions_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            "UPDATE room_members
             SET
                admin_added_permissions = $3,
                admin_removed_permissions = $4,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(added_permissions)?)
            .bind(permission_bits_to_i64(removed_permissions)?)
            .bind(current_version)
            .fetch_optional(executor)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically grant permission bits (bitwise OR in SQL to avoid read-modify-write TOCTOU)
    ///
    /// Only applies to active members (`left_at IS NULL`). Returns `NotFound` if
    /// the member has left, preventing ghost permission grants on departed users.
    pub async fn grant_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                added_permissions = added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound("Active room member not found".to_string())),
        }
    }

    /// Atomically grant permission bits for an active member that still matches the expected role.
    pub async fn grant_permission_atomic_for_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                added_permissions = added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .bind(role)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically grant admin-specific permission bits.
    ///
    /// Only applies to active members (`left_at IS NULL`). Returns `NotFound` if
    /// the member has left, preventing ghost permission grants on departed users.
    pub async fn grant_admin_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                admin_added_permissions = admin_added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound("Active room member not found".to_string())),
        }
    }

    /// Atomically grant admin-specific permission bits for an active admin that still matches the expected role.
    pub async fn grant_admin_permission_atomic_for_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                admin_added_permissions = admin_added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .bind(role)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically revoke permission bits (bitwise OR on `removed_permissions` in SQL)
    ///
    /// Only applies to active members (`left_at IS NULL`). Returns `NotFound` if
    /// the member has left, preventing ghost permission revokes on departed users.
    pub async fn revoke_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                removed_permissions = removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound("Active room member not found".to_string())),
        }
    }

    /// Atomically revoke permission bits for an active member that still matches the expected role.
    pub async fn revoke_permission_atomic_for_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                removed_permissions = removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .bind(role)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically revoke admin-specific permission bits.
    ///
    /// Only applies to active members (`left_at IS NULL`). Returns `NotFound` if
    /// the member has left, preventing ghost permission revokes on departed users.
    pub async fn revoke_admin_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                admin_removed_permissions = admin_removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound("Active room member not found".to_string())),
        }
    }

    /// Atomically revoke admin-specific permission bits for an active admin that still matches the expected role.
    pub async fn revoke_admin_permission_atomic_for_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                admin_removed_permissions = admin_removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(permission_bits_to_i64(permission)?)
            .bind(role)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Reset member permissions to role default (clear added/removed)
    pub async fn reset_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        current_version: i64,
    ) -> Result<RoomMember> {
        let sql = format!(
            "UPDATE room_members
             SET
                added_permissions = 0,
                removed_permissions = 0,
                admin_added_permissions = 0,
                admin_removed_permissions = 0,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $3
             RETURNING {ROOM_MEMBER_RETURNING_COLUMNS}"
        );
        let member = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .bind(current_version)
            .fetch_optional(&self.pool)
            .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Ban member from room.
    ///
    /// Ban is independent from lifecycle status. Active members are moved to
    /// `Left` immediately; historical rows keep their existing lifecycle. Unban
    /// clears ban metadata only and does not rejoin the user.
    pub async fn ban_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<RoomMember> {
        let now = chrono::Utc::now();
        let mut tx = self.pool.begin().await?;
        let member = self
            .ban_member_with_executor(room_id, user_id, banned_by, reason, now, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(member)
    }

    pub async fn ban_member_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
        now: chrono::DateTime<chrono::Utc>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<RoomMember> {
        let inserted = sqlx::query!(
            r"
            INSERT INTO room_member_bans (room_id, user_id, banned_by, reason, starts_at)
            SELECT rm.room_id, rm.user_id, $3, $4, $5
            FROM room_members rm
            WHERE rm.room_id = $1 AND rm.user_id = $2
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_bans rmb
                  WHERE rmb.room_id = rm.room_id
                    AND rmb.user_id = rm.user_id
                    AND rmb.revoked_at IS NULL
                    AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                  )
            ",
            room_id as &RoomId,
            user_id as &UserId,
            banned_by.map(UserId::as_i64),
            reason,
            now,
        )
        .execute(&mut **tx)
        .await?;

        if inserted.rows_affected() == 0 {
            return Err(Error::NotFound(
                "Member not found or already banned".to_string(),
            ));
        }

        sqlx::query!(
            "UPDATE room_members
             SET left_at = COALESCE(left_at, $3), version = version + 1
             WHERE room_id = $1 AND user_id = $2",
            room_id as &RoomId,
            user_id as &UserId,
            now,
        )
        .execute(&mut **tx)
        .await?;

        self.get_any_with_executor(room_id, user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound("Member not found".to_string()))
    }

    /// Unban member from room.
    ///
    /// Uses `fetch_optional` (not `fetch_one`) so that a missing or already-unbanned
    /// member returns a descriptive `NotFound` error rather than a raw sqlx
    /// `RowNotFound` panic-like error. Unban only clears ban metadata; it never
    /// restores lifecycle state or implicitly rejoins the user.
    pub async fn unban_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<RoomMember> {
        let mut tx = self.pool.begin().await?;
        let member = self
            .unban_member_with_executor(room_id, user_id, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(member)
    }

    pub async fn unban_member_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<RoomMember> {
        let result = sqlx::query!(
            "UPDATE room_member_bans
             SET revoked_at = CURRENT_TIMESTAMP
             WHERE room_id = $1 AND user_id = $2
               AND revoked_at IS NULL
               AND (ends_at IS NULL OR ends_at > CURRENT_TIMESTAMP)",
            room_id as &RoomId,
            user_id as &UserId,
        )
        .execute(&mut **tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound(
                "Member not found or not banned".to_string(),
            ));
        }

        self.get_any_with_executor(room_id, user_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound("Member not found".to_string()))
    }

    /// Check if user is an active member of room (excludes banned members)
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_members
                WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
            ) as "exists!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Check if user is banned from room
    pub async fn is_banned(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_member_bans
                WHERE room_id = $1 AND user_id = $2
                  AND revoked_at IS NULL
                  AND (ends_at IS NULL OR ends_at > CURRENT_TIMESTAMP)
            ) as "exists!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    /// Get member count for room
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i32> {
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

        Ok(i32::try_from(count).unwrap_or(i32::MAX))
    }

    /// Get member counts for multiple rooms in a single query.
    ///
    /// Returns a map from room ID string to member count. Rooms with zero
    /// members will not appear in the map.
    pub async fn count_by_rooms_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<RoomId, i32>> {
        if room_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let ids: Vec<i64> = room_ids.iter().map(|room_id| room_id.as_i64()).collect();
        let rows = sqlx::query!(
            r#"
            SELECT room_id as "room_id: RoomId", COUNT(*)::int as "member_count!"
            FROM room_members rm
            WHERE rm.room_id = ANY($1)
              AND rm.left_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_bans rmb
                  WHERE rmb.room_id = rm.room_id
                    AND rmb.user_id = rm.user_id
                    AND rmb.revoked_at IS NULL
                    AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
              )
            GROUP BY room_id
            "#,
            &ids,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut map = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            map.insert(row.room_id, row.member_count);
        }

        Ok(map)
    }

    /// Get rooms where a user is a member
    pub async fn list_by_user(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        let limit = pagination_u64_to_i64(pagination.limit());
        let offset = pagination_u64_to_i64(pagination.offset());

        // Single query using COUNT(*) OVER() window function for atomic count + fetch
        let rows = sqlx::query!(
            r#"
            SELECT rm.room_id as "room_id: RoomId", COUNT(*) OVER() as "total_count!"
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL
             ORDER BY rm.joined_at DESC
             LIMIT $2 OFFSET $3
            "#,
            user_id as &UserId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        let total_count = rows.first().map_or(0, |r| r.total_count);
        let room_ids = rows.into_iter().map(|r| r.room_id).collect();

        Ok((room_ids, total_count))
    }

    /// Get rooms where a user is a member with full room details and member count (optimized)
    /// Returns (room, role, status, `member_count`) tuples
    ///
    /// Uses `COUNT(*) OVER()` window function to atomically get total count and data
    /// in a single query, avoiding the race condition of separate COUNT + SELECT queries.
    pub async fn list_by_user_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>, i64)> {
        self.list_by_user_with_query(
            user_id,
            &MyRoomListQuery {
                pagination,
                ..Default::default()
            },
        )
        .await
    }

    /// List rooms where a user is a member with full room details and query semantics.
    pub async fn list_by_user_with_query(
        &self,
        user_id: &UserId,
        query: &MyRoomListQuery,
    ) -> Result<(Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (where_sql, _) = wb.build(4);
        let order_by = Self::build_my_room_order_by(query);
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.closed_at,
                r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                rm.role as user_role,
                CASE WHEN rm.left_at IS NULL THEN 'active' ELSE 'left' END as user_status,
                COUNT(rm2.user_id)::int as member_count,
                COUNT(*) OVER() as total_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2
                ON r.id = rm2.room_id
	               AND rm2.left_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM room_member_bans rmb2
                       WHERE rmb2.room_id = rm2.room_id
                         AND rmb2.user_id = rm2.user_id
                         AND rmb2.revoked_at IS NULL
                         AND (rmb2.ends_at IS NULL OR rmb2.ends_at > CURRENT_TIMESTAMP)
                   )
	            WHERE rm.user_id = $1 AND {where_sql}
	            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at,
	                     r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
	                     rm.role, rm.left_at, rm.joined_at
            ORDER BY {order_by}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query(&sql).bind(user_id).bind(limit).bind(offset),
            search_pattern.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;

        let total_count = rows
            .first()
            .map_or(0, |row| row.get::<i64, _>("total_count"));

        let results: Result<Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>> = rows
            .into_iter()
            .map(|row| {
                let room = crate::models::Room {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: row.try_get("created_by")?,
                    status: if row
                        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("closed_at")?
                        .is_some()
                    {
                        crate::models::RoomStatus::Closed
                    } else {
                        crate::models::RoomStatus::Active
                    },
                    is_banned: false,
                    closed_at: row.try_get("closed_at")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    last_activity_at: row.try_get("last_activity_at")?,
                    deleted_at: row.try_get("deleted_at")?,
                    version: row.try_get("version")?,
                };

                Ok((
                    room,
                    row.try_get("user_role")?,
                    match row.try_get::<String, _>("user_status")?.as_str() {
                        "left" => MemberStatus::Left,
                        _ => MemberStatus::Active,
                    },
                    row.try_get("member_count")?,
                ))
            })
            .collect();

        Ok((results?, total_count))
    }

    /// List only rooms whose creator is still active.
    pub async fn list_accessible_by_user_with_query(
        &self,
        user_id: &UserId,
        query: &MyRoomListQuery,
    ) -> Result<(Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>, i64)> {
        let limit = pagination_u64_to_i64(query.pagination.limit());
        let offset = pagination_u64_to_i64(query.pagination.offset());
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (where_sql, _) = wb.build(4);
        let order_by = Self::build_my_room_order_by(query);
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.closed_at,
                r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                rm.role as user_role,
                CASE WHEN rm.left_at IS NULL THEN 'active' ELSE 'left' END as user_status,
                COUNT(rm2.user_id)::int as member_count,
                COUNT(*) OVER() as total_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2
                ON r.id = rm2.room_id
	               AND rm2.left_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM room_member_bans rmb2
                       WHERE rmb2.room_id = rm2.room_id
                         AND rmb2.user_id = rm2.user_id
                         AND rmb2.revoked_at IS NULL
                         AND (rmb2.ends_at IS NULL OR rmb2.ends_at > CURRENT_TIMESTAMP)
                   )
	            WHERE rm.user_id = $1 AND {where_sql} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}
	            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at,
	                     r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
	                     rm.role, rm.left_at, rm.joined_at
            ORDER BY {order_by}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query(&sql).bind(user_id).bind(limit).bind(offset),
            search_pattern.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;

        let total_count = rows
            .first()
            .map_or(0, |row| row.get::<i64, _>("total_count"));

        let results: Result<Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>> = rows
            .into_iter()
            .map(|row| {
                let room = crate::models::Room {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: row.try_get("created_by")?,
                    status: if row
                        .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("closed_at")?
                        .is_some()
                    {
                        crate::models::RoomStatus::Closed
                    } else {
                        crate::models::RoomStatus::Active
                    },
                    is_banned: false,
                    closed_at: row.try_get("closed_at")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    last_activity_at: row.try_get("last_activity_at")?,
                    deleted_at: row.try_get("deleted_at")?,
                    version: row.try_get("version")?,
                };

                Ok((
                    room,
                    row.try_get("user_role")?,
                    match row.try_get::<String, _>("user_status")?.as_str() {
                        "left" => MemberStatus::Left,
                        _ => MemberStatus::Active,
                    },
                    row.try_get("member_count")?,
                ))
            })
            .collect();

        Ok((results?, total_count))
    }

    /// List all members including inactive (left) (admin view)
    pub async fn list_by_room_all(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query_as!(
            RoomMemberWithUserRow,
            r#"SELECT
                rm.room_id AS "room_id: RoomId",
                rm.user_id AS "user_id: UserId",
                rm.role AS "role: RoomRole",
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.left_at,
                EXISTS (
                    SELECT 1 FROM room_member_bans rmb
                    WHERE rmb.room_id = rm.room_id
                      AND rmb.user_id = rm.user_id
                      AND rmb.revoked_at IS NULL
                      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                ) AS "is_banned!",
                NULL::timestamptz AS "banned_at?",
                (
                    SELECT rmb.reason FROM room_member_bans rmb
                    WHERE rmb.room_id = rm.room_id
                      AND rmb.user_id = rm.user_id
                      AND rmb.revoked_at IS NULL
                      AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
                    ORDER BY rmb.starts_at DESC
                    LIMIT 1
                ) AS banned_reason,
                u.username,
                (rm.left_at IS NULL) AS "is_active!"
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC"#,
            room_id.as_i64()
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(Self::typed_row_to_member_with_user)
            .collect()
    }

    /// Atomically remove a member only if the actor has a strictly higher role.
    ///
    /// Role values in DB: 1=Creator, 2=Admin, 3=Member, 4=Guest (lower = higher authority).
    /// The WHERE clause `actor.role < target.role` ensures the actor outranks the target,
    /// eliminating the TOCTOU race between checking roles and performing the removal.
    ///
    /// Returns `Ok(true)` if the member was removed, `Ok(false)` if the target was not
    /// found or the actor does not outrank the target.
    pub async fn remove_with_role_check(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
    ) -> Result<bool> {
        self.remove_with_role_check_with_executor(room_id, actor_id, target_id, &self.pool)
            .await
    }

    pub async fn remove_with_role_check_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            "UPDATE room_members AS target
             SET left_at = $4, version = target.version + 1
             WHERE target.room_id = $1
               AND target.user_id = $3
               AND target.left_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM room_members AS actor
                   WHERE actor.room_id = $1
                     AND actor.user_id = $2
                     AND actor.left_at IS NULL
                     AND actor.role < target.role
               )",
            room_id as &RoomId,
            actor_id as &UserId,
            target_id as &UserId,
            chrono::Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Atomically ban a member only if the actor has a strictly higher role.
    ///
    /// Combines the role hierarchy check and ban update into a single SQL statement
    /// to prevent TOCTOU races. See `remove_with_role_check` for role value semantics.
    ///
    /// Returns the banned member on success. Returns `Err(NotFound)` if the target
    /// does not exist or the actor does not outrank the target.
    pub async fn ban_with_role_check(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
        reason: Option<String>,
    ) -> Result<RoomMember> {
        let now = chrono::Utc::now();
        let mut tx = self.pool.begin().await?;
        let member = self
            .ban_with_role_check_with_executor(room_id, actor_id, target_id, reason, now, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(member)
    }

    pub async fn ban_with_role_check_with_executor(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
        reason: Option<String>,
        now: chrono::DateTime<chrono::Utc>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<RoomMember> {
        let inserted = sqlx::query!(
            r"
            INSERT INTO room_member_bans (room_id, user_id, banned_by, reason, starts_at)
            SELECT target.room_id, target.user_id, $3, $4, $5
            FROM room_members AS target
            WHERE target.room_id = $1
              AND target.user_id = $2
              AND target.left_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_bans rmb
                  WHERE rmb.room_id = target.room_id
                    AND rmb.user_id = target.user_id
                    AND rmb.revoked_at IS NULL
                    AND (rmb.ends_at IS NULL OR rmb.ends_at > CURRENT_TIMESTAMP)
              )
              AND EXISTS (
                  SELECT 1 FROM room_members AS actor
                  WHERE actor.room_id = $1
                    AND actor.user_id = $3
                    AND actor.left_at IS NULL
                    AND actor.role < target.role
                  )
            ",
            room_id as &RoomId,
            target_id as &UserId,
            actor_id as &UserId,
            reason,
            now,
        )
        .execute(&mut **tx)
        .await?;

        if inserted.rows_affected() == 0 {
            return Err(Error::NotFound(
                "Target not found, already banned, or actor does not outrank target".to_string(),
            ));
        }

        sqlx::query!(
            "UPDATE room_members
             SET left_at = $3, version = version + 1
             WHERE room_id = $1 AND user_id = $2",
            room_id as &RoomId,
            target_id as &UserId,
            now,
        )
        .execute(&mut **tx)
        .await?;

        self.get_any_with_executor(room_id, target_id, &mut **tx)
            .await?
            .ok_or_else(|| Error::NotFound("Member not found".to_string()))
    }

    /// Diagnose why an `ON CONFLICT DO UPDATE... WHERE` clause did not match.
    ///
    /// Queries the existing membership row to determine if the user is banned
    /// or has already left the room, returning a semantic error.
    async fn diagnose_add_conflict<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            "SELECT {ROOM_MEMBER_RETURNING_COLUMNS}
             FROM room_members
             WHERE room_id = $1 AND user_id = $2"
        );
        let existing = sqlx::query_as::<_, RoomMember>(&sql)
            .bind(room_id)
            .bind(user_id)
            .fetch_optional(executor)
            .await?;

        match existing {
            Some(_) if self.is_banned(room_id, user_id).await? => Err(Error::Authorization(
                "User is banned from this room".to_string(),
            )),
            Some(m) if m.left_at.is_some() => Err(Error::InvalidInput(
                "User has already left this room".to_string(),
            )),
            Some(_) => {
                // Unexpected: row exists, not banned, not left — should have matched
                Err(Error::Internal(
                    "Unexpected conflict adding room member".to_string(),
                ))
            }
            None => {
                // No existing row — shouldn't happen with ON CONFLICT
                Err(Error::Internal(
                    "Unexpected state: no conflicting row found".to_string(),
                ))
            }
        }
    }

    /// Convert database row to `RoomMemberWithUser`
    fn typed_row_to_member_with_user(row: RoomMemberWithUserRow) -> Result<RoomMemberWithUser> {
        let status = if row.left_at.is_some() {
            MemberStatus::Left
        } else {
            MemberStatus::Active
        };

        Ok(RoomMemberWithUser {
            room_id: row.room_id,
            user_id: row.user_id,
            username: row.username,
            role: row.role,
            status,
            added_permissions: db_permission_i64_to_u64(
                row.added_permissions,
                "added_permissions",
            )?,
            removed_permissions: db_permission_i64_to_u64(
                row.removed_permissions,
                "removed_permissions",
            )?,
            admin_added_permissions: db_permission_i64_to_u64(
                row.admin_added_permissions,
                "admin_added_permissions",
            )?,
            admin_removed_permissions: db_permission_i64_to_u64(
                row.admin_removed_permissions,
                "admin_removed_permissions",
            )?,
            joined_at: row.joined_at,
            left_at: row.left_at,
            is_online: false,
            is_active: row.is_active,
            is_banned: row.is_banned,
            banned_at: row.banned_at,
            banned_reason: row.banned_reason,
        })
    }

    /// Convert database row to `RoomMemberWithUser`
    fn row_to_member_with_user(row: &PgRow) -> Result<RoomMemberWithUser> {
        let role: RoomRole = row.try_get("role")?;
        let left_at: Option<chrono::DateTime<chrono::Utc>> = row.try_get("left_at")?;
        let status = if left_at.is_some() {
            MemberStatus::Left
        } else {
            MemberStatus::Active
        };

        Ok(RoomMemberWithUser {
            room_id: row.try_get("room_id")?,
            user_id: row.try_get("user_id")?,
            username: row.try_get("username")?,
            role,
            status,
            added_permissions: db_permission_i64_to_u64(
                row.try_get::<i64, _>("added_permissions")?,
                "added_permissions",
            )?,
            removed_permissions: db_permission_i64_to_u64(
                row.try_get::<i64, _>("removed_permissions")?,
                "removed_permissions",
            )?,
            admin_added_permissions: db_permission_i64_to_u64(
                row.try_get::<i64, _>("admin_added_permissions")?,
                "admin_added_permissions",
            )?,
            admin_removed_permissions: db_permission_i64_to_u64(
                row.try_get::<i64, _>("admin_removed_permissions")?,
                "admin_removed_permissions",
            )?,
            joined_at: row.try_get("joined_at")?,
            left_at,
            is_online: false, // Will be populated by connection tracking
            is_active: left_at.is_none(),
            is_banned: row.try_get("is_banned")?,
            banned_at: row
                .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("banned_at")
                .ok()
                .flatten(),
            banned_reason: row.try_get("banned_reason")?,
        })
    }
}
