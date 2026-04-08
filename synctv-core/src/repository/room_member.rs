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

const ACCESSIBLE_ROOM_CREATOR_CONDITION: &str =
    "EXISTS (SELECT 1 FROM users u WHERE u.id = r.created_by AND u.deleted_at IS NULL AND u.status = 1)";

impl RoomMemberRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
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
                format!("rm.status {direction}, rm.joined_at ASC, rm.user_id ASC")
            }
        }
    }

    fn build_my_room_list_conditions(query: &MyRoomListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("rm.left_at IS NULL");
        wb.push_literal("r.deleted_at IS NULL");

        match query.status {
            Some(RoomStatus::Active) => wb.push_literal("r.status = 1"),
            Some(RoomStatus::Pending) => wb.push_literal("r.status = 2"),
            Some(RoomStatus::Rejected) => wb.push_literal("r.status = 3"),
            Some(RoomStatus::Closed) => wb.push_literal("r.status = 4"),
            None => {}
        }

        match query.is_banned {
            Some(true) => wb.push_literal("r.is_banned = TRUE"),
            Some(false) => wb.push_literal("r.is_banned = FALSE"),
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
        search_pattern: &'q Option<String>,
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
    /// rejoin freely.  When an `ON CONFLICT` row exists **and the user is not
    /// banned**, the `DO UPDATE` branch resets `left_at` to `NULL`, refreshes
    /// `joined_at`, and bumps the version.  This is the designed rejoin flow:
    /// left users can re-enter without needing an explicit invite or approval.
    ///
    /// When the `ON CONFLICT` row exists but the `DO UPDATE ... WHERE` condition
    /// is not satisfied (user is **banned**), no row is returned. In that case a
    /// follow-up query determines the specific reason and returns a semantic
    /// error (`Authorization` for banned).
    pub async fn add(&self, member: &RoomMember) -> Result<RoomMember> {
        let result = sqlx::query_as::<_, RoomMember>(
            "INSERT INTO room_members (
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                added_permissions = room_members.added_permissions,
                removed_permissions = room_members.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE room_members.status != $9
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .bind(MemberStatus::Banned)
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
        let result = sqlx::query_as::<_, RoomMember>(
            "INSERT INTO room_members (
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                added_permissions = room_members.added_permissions,
                removed_permissions = room_members.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE room_members.status != $9
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .bind(MemberStatus::Banned)
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
    /// All checks use SELECT ... FOR UPDATE to lock rows and prevent race conditions.
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
        let room_row = sqlx::query(
            "SELECT id, status FROM rooms
             WHERE id = $1
             FOR UPDATE",
        )
        .bind(member.room_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;

        let room_row = match room_row {
            Some(row) => row,
            None => return Err(Error::NotFound("Room not found".to_string())),
        };

        // 2. Check if room is active (if option enabled)
        if options.check_room_active {
            let status: i16 = room_row.try_get("status")?;
            if status != 1 {
                // 1 = Active
                return Err(Error::InvalidInput("Room is not active".to_string()));
            }
        }

        // 3. Check if user is already a member (if option enabled)
        if options.check_duplicate {
            let existing = sqlx::query(
                "SELECT user_id FROM room_members
                 WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
                 FOR UPDATE",
            )
            .bind(member.room_id.as_str())
            .bind(member.user_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;

            if existing.is_some() {
                return Err(Error::AlreadyExists(
                    "Already a member of this room".to_string(),
                ));
            }
        }

        // 4. Check max members limit (if option enabled)
        //    When max_members is 0 or None, treat as unlimited (no enforcement).
        //
        //    IMPORTANT (Task #47): We use a subquery with FOR UPDATE to lock all
        //    member rows, then count them. This prevents TOCTOU races where two
        //    concurrent transactions could both see count < max and both insert,
        //    exceeding the limit. PostgreSQL doesn't allow FOR UPDATE directly
        //    with aggregate functions like COUNT(*).
        if options.check_max_members {
            let max_members = options.max_members;
            if max_members > 0 {
                sqlx::query("SELECT id FROM rooms WHERE id = $1 FOR UPDATE")
                    .bind(member.room_id.as_str())
                    .execute(&mut **tx)
                    .await?;

                // Lock all member rows for this room to prevent concurrent inserts
                // from seeing the same count value
                let count_row = sqlx::query(
                    "SELECT COUNT(*) as count FROM (
                        SELECT 1 FROM room_members
                        WHERE room_id = $1 AND left_at IS NULL AND status = $2
                        FOR UPDATE
                    ) sub",
                )
                .bind(member.room_id.as_str())
                .bind(MemberStatus::Active)
                .fetch_one(&mut **tx)
                .await?;

                let count: i64 = count_row.try_get("count")?;
                if count as u64 >= max_members {
                    tracing::warn!(
                        room_id = %member.room_id.as_str(),
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
        let result = sqlx::query_as::<_, RoomMember>(
            "INSERT INTO room_members (
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                joined_at, version
             )
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET
                role = EXCLUDED.role,
                status = EXCLUDED.status,
                added_permissions = room_members.added_permissions,
                removed_permissions = room_members.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             WHERE room_members.status != $9
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .bind(MemberStatus::Banned)
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
        let result = sqlx::query(
            "UPDATE room_members
             SET status = $3, left_at = $2, version = version + 1
             WHERE user_id = $1 AND left_at IS NULL",
        )
        .bind(user_id.as_str())
        .bind(chrono::Utc::now())
        .bind(MemberStatus::Left)
        .execute(executor)
        .await?;

        Ok(result.rows_affected())
    }

    /// Remove user from room (soft delete - set `status = Left` and `left_at`)
    pub async fn remove(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE room_members
             SET status = $4, left_at = $3, version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(chrono::Utc::now())
        .bind(MemberStatus::Left)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get member by room and user
    pub async fn get(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let member = sqlx::query_as::<_, RoomMember>(
            "SELECT
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason
             FROM room_members
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(member)
    }

    /// Get member by ID (including banned/inactive)
    pub async fn get_any(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let member = sqlx::query_as::<_, RoomMember>(
            "SELECT
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason
             FROM room_members
             WHERE room_id = $1 AND user_id = $2",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(member)
    }

    /// List all active members in a room
    pub async fn list_by_room(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query(
            "SELECT
                rm.room_id, rm.user_id, rm.role, rm.status,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.banned_at, rm.banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND rm.left_at IS NULL AND rm.status != $2 AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC",
        )
        .bind(room_id.as_str())
        .bind(MemberStatus::Banned)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| self.row_to_member_with_user(row))
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
    /// - Banned members (`status = Banned`)
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
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));

        let mut count_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT COUNT(*) FROM room_members rm JOIN users u ON rm.user_id = u.id WHERE rm.room_id = ",
        );
        count_builder.push_bind(room_id.as_str());
        match query.status {
            Some(MemberStatus::Banned | MemberStatus::Rejected | MemberStatus::Left) => {
                count_builder.push(" AND rm.status = ");
                count_builder.push_bind(query.status.expect("status checked above"));
            }
            Some(status) => {
                count_builder.push(" AND rm.left_at IS NULL AND rm.status = ");
                count_builder.push_bind(status);
            }
            None => {
                count_builder.push(" AND rm.left_at IS NULL AND rm.status != ");
                count_builder.push_bind(MemberStatus::Banned);
            }
        }
        count_builder.push(" AND u.deleted_at IS NULL");
        if let Some(pattern) = &search_pattern {
            count_builder.push(" AND (u.username ILIKE ");
            count_builder.push_bind(pattern);
            count_builder.push(" OR rm.user_id ILIKE ");
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
                rm.room_id, rm.user_id, rm.role, rm.status,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.banned_at, rm.banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = ",
        );
        list_builder.push_bind(room_id.as_str());
        match query.status {
            Some(MemberStatus::Banned | MemberStatus::Rejected | MemberStatus::Left) => {
                list_builder.push(" AND rm.status = ");
                list_builder.push_bind(query.status.expect("status checked above"));
            }
            Some(status) => {
                list_builder.push(" AND rm.left_at IS NULL AND rm.status = ");
                list_builder.push_bind(status);
            }
            None => {
                list_builder.push(" AND rm.left_at IS NULL AND rm.status != ");
                list_builder.push_bind(MemberStatus::Banned);
            }
        }
        list_builder.push(" AND u.deleted_at IS NULL");
        if let Some(pattern) = &search_pattern {
            list_builder.push(" AND (u.username ILIKE ");
            list_builder.push_bind(pattern);
            list_builder.push(" OR rm.user_id ILIKE ");
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
            .map(|row| self.row_to_member_with_user(row))
            .collect();

        Ok((members?, total_count))
    }

    /// List all active members in a room with online status
    pub async fn list_by_room_with_online(
        &self,
        room_id: &RoomId,
        online_user_ids: &[UserId],
    ) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query(
            "SELECT
                rm.room_id, rm.user_id, rm.role, rm.status,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.banned_at, rm.banned_reason,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND rm.left_at IS NULL AND rm.status != $2 AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC",
        )
        .bind(room_id.as_str())
        .bind(MemberStatus::Banned)
        .fetch_all(&self.pool)
        .await?;

        let online_set: std::collections::HashSet<_> = online_user_ids
            .iter()
            .map(super::super::models::id::UserId::as_str)
            .collect();

        rows.into_iter()
            .map(|row| {
                let mut member = self.row_to_member_with_user(row)?;
                member.is_online = online_set.contains(member.user_id.as_str());
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                role = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(role)
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member role inside an existing transaction.
    pub async fn update_role_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        executor: impl sqlx::PgExecutor<'_>,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                role = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(role)
        .fetch_optional(executor)
        .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound(
                "User is not an active member of this room".to_string(),
            )),
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                status = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(status)
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Reject a pending member or invitation while preserving an auditable row.
    pub async fn reject_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        current_version: i64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                status = $3,
                left_at = CURRENT_TIMESTAMP,
                banned_at = NULL,
                banned_by = NULL,
                banned_reason = NULL,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Rejected)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                added_permissions = $3,
                removed_permissions = $4,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(added_permissions as i64)
        .bind(removed_permissions as i64)
        .bind(current_version)
        .fetch_optional(&self.pool)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                admin_added_permissions = $3,
                admin_removed_permissions = $4,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(added_permissions as i64)
        .bind(removed_permissions as i64)
        .bind(current_version)
        .fetch_optional(&self.pool)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                added_permissions = added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                added_permissions = added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                admin_added_permissions = admin_added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                admin_added_permissions = admin_added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                removed_permissions = removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                removed_permissions = removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                admin_removed_permissions = admin_removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                admin_removed_permissions = admin_removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND role = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                added_permissions = 0,
                removed_permissions = 0,
                admin_added_permissions = 0,
                admin_removed_permissions = 0,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $3
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Ban member from room
    ///
    /// Only bans members that are not already banned (`status != Banned`),
    /// preserving the original ban audit info (`banned_at`, `banned_by`, `banned_reason`).
    pub async fn ban_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        banned_by: &UserId,
        reason: Option<String>,
    ) -> Result<RoomMember> {
        let result = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                status = $3,
                left_at = CURRENT_TIMESTAMP,
                banned_at = $4,
                banned_by = $5,
                banned_reason = $6,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND status != $3
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Banned)
        .bind(chrono::Utc::now())
        .bind(banned_by.as_str())
        .bind(reason)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(m) => Ok(m),
            None => Err(Error::NotFound(
                "Member not found or already banned".to_string(),
            )),
        }
    }

    /// Unban member from room
    ///
    /// Uses `fetch_optional` (not `fetch_one`) so that a missing or already-unbanned
    /// member returns a descriptive `NotFound` error rather than a raw sqlx
    /// `RowNotFound` panic-like error.
    pub async fn unban_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<RoomMember> {
        let result = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members
             SET
                status = $3,
                left_at = NULL,
                banned_at = NULL,
                banned_by = NULL,
                banned_reason = NULL,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND status = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Active)
        .bind(MemberStatus::Banned)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(m) => Ok(m),
            None => Err(Error::NotFound(
                "Member not found or not banned".to_string(),
            )),
        }
    }

    /// Check if user is an active member of room (excludes banned members)
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND status = $3",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Active)
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Check if user is banned from room
    pub async fn is_banned(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members
             WHERE room_id = $1 AND user_id = $2 AND status = $3",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Banned)
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Get member count for room
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i32> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members
             WHERE room_id = $1 AND left_at IS NULL AND status = $2",
        )
        .bind(room_id.as_str())
        .bind(MemberStatus::Active)
        .fetch_one(&self.pool)
        .await?;

        Ok(i32::try_from(count).unwrap_or(i32::MAX))
    }

    /// Get member counts for multiple rooms in a single query.
    ///
    /// Returns a map from room ID string to member count. Rooms with zero
    /// members will not appear in the map.
    pub async fn count_by_rooms_batch(
        &self,
        room_ids: &[&RoomId],
    ) -> Result<std::collections::HashMap<String, i32>> {
        if room_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let ids: Vec<&str> = room_ids.iter().map(|r| r.as_str()).collect();
        let rows = sqlx::query(
            "SELECT room_id, COUNT(*)::int as member_count
             FROM room_members
             WHERE room_id = ANY($1) AND left_at IS NULL AND status = $2
             GROUP BY room_id",
        )
        .bind(&ids)
        .bind(MemberStatus::Active)
        .fetch_all(&self.pool)
        .await?;

        let mut map = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            let room_id: String = row.try_get("room_id")?;
            let count: i32 = row.try_get("member_count")?;
            map.insert(room_id, count);
        }

        Ok(map)
    }

    /// Get rooms where a user is a member
    pub async fn list_by_user(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<RoomId>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Single query using COUNT(*) OVER() window function for atomic count + fetch
        let rows = sqlx::query(
            "SELECT rm.room_id, COUNT(*) OVER() as total_count
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL
             ORDER BY rm.joined_at DESC
             LIMIT $2 OFFSET $3",
        )
        .bind(user_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let total_count = rows.first().map_or(0, |r| r.get::<i64, _>("total_count"));
        let room_ids = rows
            .into_iter()
            .map(|r| RoomId::from_string(r.get::<String, _>("room_id")))
            .collect();

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
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (where_sql, _) = wb.build(4);
        let order_by = Self::build_my_room_order_by(query);
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status,
                r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                rm.role as user_role,
                rm.status as user_status,
                COUNT(rm2.user_id)::int as member_count,
                COUNT(*) OVER() as total_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2 ON r.id = rm2.room_id AND rm2.left_at IS NULL
            WHERE rm.user_id = $1 AND {where_sql}
            GROUP BY r.id, r.name, r.description, r.created_by, r.status,
                     r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                     rm.role, rm.status, rm.joined_at
            ORDER BY {order_by}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query(&sql)
                .bind(user_id.as_str())
                .bind(limit)
                .bind(offset),
            &search_pattern,
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
                    id: RoomId::from_string(row.try_get("id")?),
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: UserId::from_string(row.try_get("created_by")?),
                    status: row.try_get("status")?,
                    is_banned: row.try_get("is_banned")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    last_activity_at: row.try_get("last_activity_at")?,
                    deleted_at: row.try_get("deleted_at")?,
                    version: row.try_get("version")?,
                };

                Ok((
                    room,
                    row.try_get("user_role")?,
                    row.try_get("user_status")?,
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
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (where_sql, _) = wb.build(4);
        let order_by = Self::build_my_room_order_by(query);
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status,
                r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                rm.role as user_role,
                rm.status as user_status,
                COUNT(rm2.user_id)::int as member_count,
                COUNT(*) OVER() as total_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2 ON r.id = rm2.room_id AND rm2.left_at IS NULL
            WHERE rm.user_id = $1 AND {where_sql} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}
            GROUP BY r.id, r.name, r.description, r.created_by, r.status,
                     r.is_banned, r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                     rm.role, rm.status, rm.joined_at
            ORDER BY {order_by}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query(&sql)
                .bind(user_id.as_str())
                .bind(limit)
                .bind(offset),
            &search_pattern,
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
                    id: RoomId::from_string(row.try_get("id")?),
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: UserId::from_string(row.try_get("created_by")?),
                    status: row.try_get("status")?,
                    is_banned: row.try_get("is_banned")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    last_activity_at: row.try_get("last_activity_at")?,
                    deleted_at: row.try_get("deleted_at")?,
                    version: row.try_get("version")?,
                };

                Ok((
                    room,
                    row.try_get("user_role")?,
                    row.try_get("user_status")?,
                    row.try_get("member_count")?,
                ))
            })
            .collect();

        Ok((results?, total_count))
    }

    /// List all members including inactive (left) (admin view)
    pub async fn list_by_room_all(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query(
            "SELECT
                rm.room_id, rm.user_id, rm.role, rm.status,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at, rm.banned_at, rm.banned_reason,
                rm.banned_by, rm.left_at, rm.version,
                u.username,
                CASE WHEN rm.left_at IS NULL THEN true ELSE false END as is_active
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1 AND u.deleted_at IS NULL
             ORDER BY rm.joined_at ASC",
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let is_active: bool = row.try_get("is_active")?;
                let mut member = self.row_to_member_with_user(row)?;
                member.is_active = is_active;
                // is_online stays false — this method doesn't have WebSocket status info
                Ok(member)
            })
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
        let result = sqlx::query(
            "UPDATE room_members AS target
             SET status = $5, left_at = $4, version = target.version + 1
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
        )
        .bind(room_id.as_str())
        .bind(actor_id.as_str())
        .bind(target_id.as_str())
        .bind(chrono::Utc::now())
        .bind(MemberStatus::Left)
        .execute(&self.pool)
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
        let member = sqlx::query_as::<_, RoomMember>(
            "UPDATE room_members AS target
             SET
                status = $4,
                left_at = CURRENT_TIMESTAMP,
                banned_at = $5,
                banned_by = $6,
                banned_reason = $7,
                version = target.version + 1
             WHERE target.room_id = $1
               AND target.user_id = $3
               AND target.left_at IS NULL
               AND target.status != $4
               AND EXISTS (
                   SELECT 1 FROM room_members AS actor
                   WHERE actor.room_id = $1
                     AND actor.user_id = $2
                     AND actor.left_at IS NULL
                     AND actor.role < target.role
               )
             RETURNING
                target.room_id, target.user_id, target.role, target.status,
                target.added_permissions, target.removed_permissions,
                target.admin_added_permissions, target.admin_removed_permissions,
                target.joined_at, target.left_at, target.version,
                target.banned_at, target.banned_by, target.banned_reason",
        )
        .bind(room_id.as_str())
        .bind(actor_id.as_str())
        .bind(target_id.as_str())
        .bind(MemberStatus::Banned)
        .bind(chrono::Utc::now())
        .bind(actor_id.as_str())
        .bind(reason)
        .fetch_optional(&self.pool)
        .await?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::NotFound(
                "Target not found, already banned, or actor does not outrank target".to_string(),
            )),
        }
    }

    /// Diagnose why an `ON CONFLICT DO UPDATE ... WHERE` clause did not match.
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
        let existing = sqlx::query_as::<_, RoomMember>(
            "SELECT
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason
             FROM room_members
             WHERE room_id = $1 AND user_id = $2",
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(executor)
        .await?;

        match existing {
            Some(m) if m.status == MemberStatus::Banned => Err(Error::Authorization(
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
    fn row_to_member_with_user(&self, row: PgRow) -> Result<RoomMemberWithUser> {
        let role: RoomRole = row.try_get("role")?;
        let status: MemberStatus = row.try_get("status")?;

        Ok(RoomMemberWithUser {
            room_id: RoomId::from_string(row.try_get("room_id")?),
            user_id: UserId::from_string(row.try_get("user_id")?),
            username: row.try_get("username")?,
            role,
            status,
            added_permissions: row.try_get::<i64, _>("added_permissions")? as u64,
            removed_permissions: row.try_get::<i64, _>("removed_permissions")? as u64,
            admin_added_permissions: row.try_get::<i64, _>("admin_added_permissions")? as u64,
            admin_removed_permissions: row.try_get::<i64, _>("admin_removed_permissions")? as u64,
            joined_at: row.try_get("joined_at")?,
            is_online: false, // Will be populated by connection tracking
            is_active: true,  // Default; overridden by callers that have membership status info
            banned_at: row.try_get("banned_at")?,
            banned_reason: row.try_get("banned_reason")?,
        })
    }
}
