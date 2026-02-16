use sqlx::{PgPool, postgres::PgRow, Row};

use crate::{
    models::{
        RoomMember, RoomMemberWithUser, RoomId, UserId, RoomRole, MemberStatus, PageParams,
    },
    service::AddMemberOptions,
    Error, Result,
};

/// Room member repository for database operations
#[derive(Clone)]
pub struct RoomMemberRepository {
    pool: PgPool,
}

impl RoomMemberRepository {
    #[must_use] 
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Add user to room with role
    pub async fn add(&self, member: &RoomMember) -> Result<RoomMember> {
        let row = sqlx::query(
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
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .fetch_one(&self.pool)
        .await?;

        self.row_to_member(row)
    }

    /// Add user to room using a provided executor (pool or transaction)
    pub async fn add_with_executor<'e, E>(&self, member: &RoomMember, executor: E) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query(
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
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .fetch_one(executor)
        .await?;

        self.row_to_member(row)
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
             FOR UPDATE"
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
            if status != 1 {  // 1 = Active
                return Err(Error::InvalidInput("Room is not active".to_string()));
            }
        }

        // 3. Check if user is already a member (if option enabled)
        if options.check_duplicate {
            let existing = sqlx::query(
                "SELECT user_id FROM room_members
                 WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
                 FOR UPDATE"
            )
            .bind(member.room_id.as_str())
            .bind(member.user_id.as_str())
            .fetch_optional(&mut **tx)
            .await?;

            if existing.is_some() {
                return Err(Error::AlreadyExists("Already a member of this room".to_string()));
            }
        }

        // 4. Check max members limit (if option enabled)
        if options.check_max_members {
            let max_members = options.max_members;
            if max_members > 0 {
                let count_row = sqlx::query(
                    "SELECT COUNT(*) as count FROM room_members
                     WHERE room_id = $1 AND left_at IS NULL"
                )
                .bind(member.room_id.as_str())
                .fetch_one(&mut **tx)
                .await?;

                let count: i64 = count_row.try_get("count")?;
                if count as u64 >= max_members {
                    return Err(Error::InvalidInput("Room is full".to_string()));
                }
            }
        }

        // 5. Insert the new member
        let row = sqlx::query(
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
                added_permissions = EXCLUDED.added_permissions,
                removed_permissions = EXCLUDED.removed_permissions,
                left_at = NULL,
                joined_at = EXCLUDED.joined_at,
                version = room_members.version + 1
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(member.room_id.as_str())
        .bind(member.user_id.as_str())
        .bind(member.role)
        .bind(member.status)
        .bind(member.added_permissions as i64)
        .bind(member.removed_permissions as i64)
        .bind(member.joined_at)
        .bind(member.version)
        .fetch_one(&mut **tx)
        .await?;

        self.row_to_member(row)
    }

    /// Remove a user from all rooms (soft delete - set `left_at` on all active memberships).
    ///
    /// Used during user deletion/ban to clean up room memberships.
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE room_members
             SET left_at = $2, version = version + 1
             WHERE user_id = $1 AND left_at IS NULL"
        )
        .bind(user_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Remove user from room (soft delete - set `left_at`)
    pub async fn remove(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE room_members
             SET left_at = $3, version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(chrono::Utc::now())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Get member by room and user
    pub async fn get(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let row = sqlx::query(
            "SELECT
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason
             FROM room_members
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.row_to_member(row)?)),
            None => Ok(None),
        }
    }

    /// Get member by ID (including banned/inactive)
    pub async fn get_any(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let row = sqlx::query(
            "SELECT
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason
             FROM room_members
             WHERE room_id = $1 AND user_id = $2"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.row_to_member(row)?)),
            None => Ok(None),
        }
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
             WHERE rm.room_id = $1 AND rm.left_at IS NULL
             ORDER BY rm.joined_at ASC"
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| self.row_to_member_with_user(row))
            .collect()
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
             WHERE rm.room_id = $1 AND rm.left_at IS NULL
             ORDER BY rm.joined_at ASC"
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        let online_set: std::collections::HashSet<_> =
            online_user_ids.iter().map(super::super::models::id::UserId::as_str).collect();

        rows.into_iter()
            .map(|row| {
                let mut member = self.row_to_member_with_user(row)?;
                member.is_online = online_set.contains(member.user_id.as_str());
                Ok(member)
            })
            .collect()
    }

    /// Update member role with optimistic locking
    pub async fn update_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                role = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(role)
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => self.row_to_member(row),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member status with optimistic locking
    pub async fn update_status(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        status: MemberStatus,
        current_version: i64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                status = $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(status)
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => self.row_to_member(row),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member Allow/Deny permissions with optimistic locking
    pub async fn update_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                added_permissions = $3,
                removed_permissions = $4,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(added_permissions as i64)
        .bind(removed_permissions as i64)
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => self.row_to_member(row),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically grant permission bits (bitwise OR in SQL to avoid read-modify-write TOCTOU)
    pub async fn grant_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                added_permissions = added_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
        .fetch_one(&self.pool)
        .await?;

        self.row_to_member(row)
    }

    /// Atomically revoke permission bits (bitwise OR on `removed_permissions` in SQL)
    pub async fn revoke_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                removed_permissions = removed_permissions | $3,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(permission as i64)
        .fetch_one(&self.pool)
        .await?;

        self.row_to_member(row)
    }

    /// Reset member permissions to role default (clear added/removed)
    pub async fn reset_permissions(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        current_version: i64,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
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
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(current_version)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => self.row_to_member(row),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Ban member from room
    pub async fn ban_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        banned_by: &UserId,
        reason: Option<String>,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                status = $3,
                banned_at = $4,
                banned_by = $5,
                banned_reason = $6,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Banned)
        .bind(chrono::Utc::now())
        .bind(banned_by.as_str())
        .bind(reason)
        .fetch_one(&self.pool)
        .await?;

        self.row_to_member(row)
    }

    /// Unban member from room
    pub async fn unban_member(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<RoomMember> {
        let row = sqlx::query(
            "UPDATE room_members
             SET
                status = $3,
                banned_at = NULL,
                banned_by = NULL,
                banned_reason = NULL,
                version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL
             RETURNING
                room_id, user_id, role, status,
                added_permissions, removed_permissions,
                admin_added_permissions, admin_removed_permissions,
                joined_at, left_at, version,
                banned_at, banned_by, banned_reason"
        )
        .bind(room_id.as_str())
        .bind(user_id.as_str())
        .bind(MemberStatus::Active)
        .fetch_one(&self.pool)
        .await?;

        self.row_to_member(row)
    }

    /// Check if user is an active member of room (excludes banned members)
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members
             WHERE room_id = $1 AND user_id = $2 AND left_at IS NULL AND status = $3"
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
             WHERE room_id = $1 AND user_id = $2 AND status = $3"
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
             WHERE room_id = $1 AND left_at IS NULL"
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(count as i32)
    }

    /// Get rooms where a user is a member
    pub async fn list_by_user(&self, user_id: &UserId, pagination: PageParams) -> Result<(Vec<RoomId>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Get total count
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL"
        )
        .bind(user_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get room IDs
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT rm.room_id
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL
             ORDER BY rm.joined_at DESC
             LIMIT $2 OFFSET $3"
        )
        .bind(user_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let room_ids = rows.into_iter().map(RoomId::from_string).collect();

        Ok((room_ids, count))
    }

    /// Get rooms where a user is a member with full room details and member count (optimized)
    /// Returns (room, role, status, `member_count`) tuples
    pub async fn list_by_user_with_details(
        &self,
        user_id: &UserId,
        pagination: PageParams,
    ) -> Result<(Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>, i64)> {
        let limit = pagination.limit() as i64;
        let offset = pagination.offset() as i64;

        // Get total count
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) as count
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL"
        )
        .bind(user_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        // Get rooms with user role and member count using LEFT JOIN + GROUP BY
        // (avoids N+1 correlated subquery that ran COUNT per room)
        let rows = sqlx::query(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.status,
                r.is_banned, r.created_at, r.updated_at, r.deleted_at,
                rm.role as user_role,
                rm.status as user_status,
                COUNT(rm2.user_id)::int as member_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2 ON r.id = rm2.room_id AND rm2.left_at IS NULL
            WHERE rm.user_id = $1 AND rm.left_at IS NULL AND r.deleted_at IS NULL
            GROUP BY r.id, r.name, r.description, r.created_by, r.status,
                     r.is_banned, r.created_at, r.updated_at, r.deleted_at,
                     rm.role, rm.status, rm.joined_at
            ORDER BY rm.joined_at DESC
            LIMIT $2 OFFSET $3
            "
        )
        .bind(user_id.as_str())
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;

        let results: Result<Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>> = rows
            .into_iter()
            .map(|row| {
                // RoomStatus, RoomRole, MemberStatus are stored as SMALLINT (i16) in PostgreSQL
                // and have sqlx::Decode impls — read them directly as the correct types
                let status: crate::models::RoomStatus = row.try_get("status")?;

                let room = crate::models::Room {
                    id: RoomId::from_string(row.try_get("id")?),
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    created_by: UserId::from_string(row.try_get("created_by")?),
                    status,
                    is_banned: row.try_get("is_banned")?,
                    created_at: row.try_get("created_at")?,
                    updated_at: row.try_get("updated_at")?,
                    deleted_at: row.try_get("deleted_at")?,
                };

                let role: RoomRole = row.try_get("user_role")?;
                let member_status: MemberStatus = row.try_get("user_status")?;
                let member_count: i32 = row.try_get("member_count")?;

                Ok((room, role, member_status, member_count))
            })
            .collect();

        Ok((results?, count))
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
             WHERE rm.room_id = $1
             ORDER BY rm.joined_at ASC"
        )
        .bind(room_id.as_str())
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let is_active: bool = row.try_get("is_active")?;
                let mut member = self.row_to_member_with_user(row)?;
                member.is_online = is_active;
                Ok(member)
            })
            .collect()
    }

    /// Convert database row to `RoomMember`
    fn row_to_member(&self, row: PgRow) -> Result<RoomMember> {
        let role: RoomRole = row.try_get("role")?;
        let status: MemberStatus = row.try_get("status")?;

        let banned_by: Option<String> = row.try_get("banned_by")?;

        Ok(RoomMember {
            room_id: RoomId::from_string(row.try_get("room_id")?),
            user_id: UserId::from_string(row.try_get("user_id")?),
            role,
            status,
            added_permissions: row.try_get::<i64, _>("added_permissions")? as u64,
            removed_permissions: row.try_get::<i64, _>("removed_permissions")? as u64,
            admin_added_permissions: row.try_get::<i64, _>("admin_added_permissions")? as u64,
            admin_removed_permissions: row.try_get::<i64, _>("admin_removed_permissions")? as u64,
            joined_at: row.try_get("joined_at")?,
            left_at: row.try_get("left_at")?,
            version: row.try_get("version")?,
            banned_at: row.try_get("banned_at")?,
            banned_by: banned_by.map(UserId::from_string),
            banned_reason: row.try_get("banned_reason")?,
        })
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
            banned_at: row.try_get("banned_at")?,
            banned_reason: row.try_get("banned_reason")?,
        })
    }
}

#[cfg(test)]
mod tests {

    #[tokio::test]
    async fn test_add_member() {
        // Placeholder: requires full service layer setup with user/room creation
        // Covered by higher-level integration tests in synctv-core/tests/
    }
}
