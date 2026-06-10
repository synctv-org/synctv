use sqlx::PgPool;

use crate::{
    models::{
        AddMemberOptions, MemberStatus, MyRoomListQuery, MyRoomListSortBy, MyRoomRelation,
        PageParams, RoomId, RoomMember, RoomMemberListQuery, RoomMemberListSortBy,
        RoomMemberWithUser, RoomRole, RoomStatus, UserId,
    },
    repository::query_builder::trusted_dynamic_sql,
    Error, Result,
};

pub const KICK_COOLDOWN_DENIED_MESSAGE: &str =
    "User was recently kicked from this room and cannot access it yet";

use super::query_builder::{escape_ilike, WhereClauseBuilder};

pub struct KickCooldownInsert<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub kicked_by: Option<&'a UserId>,
    pub starts_at: chrono::DateTime<chrono::Utc>,
    pub ends_at: chrono::DateTime<chrono::Utc>,
    pub reason: Option<&'a str>,
}

/// Room member repository for database operations
#[derive(Clone)]
pub struct RoomMemberRepository {
    pool: PgPool,
}

#[derive(Debug, sqlx::FromRow)]
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
    is_active: bool,
}

#[derive(Debug, Clone, Copy)]
struct RoomMemberRow {
    room_id: RoomId,
    user_id: UserId,
    role: RoomRole,
    added_permissions: i64,
    removed_permissions: i64,
    admin_added_permissions: i64,
    admin_removed_permissions: i64,
    joined_at: chrono::DateTime<chrono::Utc>,
    version: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct MyRoomListRow {
    id: RoomId,
    name: String,
    description: String,
    created_by: UserId,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
    version: i32,
    last_activity_at: chrono::DateTime<chrono::Utc>,
    is_banned: bool,
    user_role: RoomRole,
    member_count: i32,
}

const ACCESSIBLE_ROOM_CREATOR_CONDITION: &str =
    "EXISTS (SELECT 1 FROM users u WHERE u.id = r.created_by AND u.deleted_at IS NULL
        AND NOT EXISTS (
            SELECT 1 FROM user_bans ub
            WHERE ub.user_id = u.id
              AND ub.revoked_at IS NULL
              AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
        ))";
const ACTIVE_ROOM_BAN_EXISTS_SQL: &str = "EXISTS (
    SELECT 1 FROM room_bans rb
    WHERE rb.room_id = r.id
      AND rb.revoked_at IS NULL
      AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
)";

pub struct MemberPermissionExactVersionUpdate<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub current_version: i64,
    pub new_version: i64,
}

pub struct MemberRolePermissionExactVersionUpdate<'a> {
    pub room_id: &'a RoomId,
    pub user_id: &'a UserId,
    pub role: RoomRole,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub use_admin_permissions: bool,
    pub current_version: i64,
    pub new_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RemovedRoomMember {
    pub room_id: RoomId,
    pub user_id: UserId,
    pub version: i64,
}
const ACTIVE_ROOM_BAN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM room_bans rb
    WHERE rb.room_id = r.id
      AND rb.revoked_at IS NULL
      AND (rb.ends_at IS NULL OR rb.ends_at > CURRENT_TIMESTAMP)
)";
const ACTIVE_KICK_COOLDOWN_NOT_EXISTS_SQL: &str = "NOT EXISTS (
    SELECT 1 FROM room_member_kick_cooldowns rmkc
    WHERE rmkc.room_id = rm.room_id
      AND rmkc.user_id = rm.user_id
      AND rmkc.ends_at > CURRENT_TIMESTAMP
)";
fn permission_bits_to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        Error::InvalidInput(format!(
            "permission bits value {value} exceeds signed storage range"
        ))
    })
}

fn db_permission_i64_to_u64(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        Error::Internal(format!(
            "database returned negative permission bits for column {column}"
        ))
    })
}

fn count_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        Error::Internal(format!(
            "database returned negative count for {field}: {value}"
        ))
    })
}

fn required_count(value: Option<i64>, context: &'static str) -> Result<i64> {
    value.ok_or_else(|| Error::Internal(format!("{context} query did not return a count")))
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

    pub async fn lifecycle_version(&self, room_id: &RoomId, user_id: &UserId) -> Result<i64> {
        self.lifecycle_version_with_executor(room_id, user_id, &self.pool)
            .await
    }

    pub async fn lifecycle_version_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<i64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let version = sqlx::query_scalar!(
            "SELECT version FROM room_member_versions WHERE room_id = $1 AND user_id = $2",
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(executor)
        .await?;
        Ok(version.unwrap_or(0))
    }

    pub async fn active_member_version_for_update_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: &mut sqlx::PgConnection,
    ) -> Result<Option<i64>> {
        let version = sqlx::query_scalar!(
            "SELECT version
             FROM room_members
             WHERE room_id = $1 AND user_id = $2
             FOR UPDATE",
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(executor)
        .await?;
        Ok(version)
    }

    pub async fn next_lifecycle_version_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<i64>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let version = sqlx::query_scalar!(
            "INSERT INTO room_member_versions (room_id, user_id, version, is_member, updated_at)
             VALUES ($1, $2, 1, FALSE, CURRENT_TIMESTAMP)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET version = room_member_versions.version + 1,
                 is_member = FALSE,
                 updated_at = CURRENT_TIMESTAMP
             RETURNING version",
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(executor)
        .await?;
        Ok(version)
    }

    pub async fn mark_active_lifecycle_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        version: i64,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query!(
            "INSERT INTO room_member_versions (room_id, user_id, version, is_member, updated_at)
             VALUES ($1, $2, $3, TRUE, CURRENT_TIMESTAMP)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET version = GREATEST(room_member_versions.version, EXCLUDED.version),
                 is_member = TRUE,
                 updated_at = CURRENT_TIMESTAMP",
            room_id as &RoomId,
            user_id as &UserId,
            version,
        )
        .execute(executor)
        .await?;
        Ok(())
    }

    fn room_member_order_by_sql(query: &RoomMemberListQuery) -> &'static str {
        use crate::models::SortDirection;

        match (query.sort_by, query.sort_direction) {
            (RoomMemberListSortBy::JoinedAt, SortDirection::Asc) => {
                "rm.joined_at ASC, rm.user_id ASC"
            }
            (RoomMemberListSortBy::JoinedAt, SortDirection::Desc) => {
                "rm.joined_at DESC, rm.user_id ASC"
            }
            (RoomMemberListSortBy::Username, SortDirection::Asc) => {
                "u.username ASC, rm.user_id ASC"
            }
            (RoomMemberListSortBy::Username, SortDirection::Desc) => {
                "u.username DESC, rm.user_id ASC"
            }
            (RoomMemberListSortBy::Role, SortDirection::Asc) => {
                "rm.role ASC, rm.joined_at ASC, rm.user_id ASC"
            }
            (RoomMemberListSortBy::Role, SortDirection::Desc) => {
                "rm.role DESC, rm.joined_at ASC, rm.user_id ASC"
            }
        }
    }

    fn push_room_member_order_by(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &RoomMemberListQuery,
    ) {
        builder.push(" ORDER BY ");
        builder.push(Self::room_member_order_by_sql(query));
    }

    fn build_my_room_list_conditions(query: &MyRoomListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal(ACTIVE_KICK_COOLDOWN_NOT_EXISTS_SQL);
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
        qb: sqlx::query::QueryAs<'q, sqlx::Postgres, MyRoomListRow, sqlx::postgres::PgArguments>,
        search_pattern: Option<&'q str>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, MyRoomListRow, sqlx::postgres::PgArguments> {
        match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        }
    }

    fn bind_my_room_count_filters<'q>(
        qb: sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments>,
        search_pattern: Option<&'q str>,
    ) -> sqlx::query::QueryScalar<'q, sqlx::Postgres, i64, sqlx::postgres::PgArguments> {
        match search_pattern {
            Some(pattern) => qb.bind(pattern),
            None => qb,
        }
    }

    fn my_room_order_by_sql(query: &MyRoomListQuery) -> &'static str {
        use crate::models::SortDirection;

        match (query.sort_by, query.sort_direction) {
            (MyRoomListSortBy::JoinedAt, SortDirection::Asc) => "rm.joined_at ASC, r.id ASC",
            (MyRoomListSortBy::JoinedAt, SortDirection::Desc) => "rm.joined_at DESC, r.id DESC",
            (MyRoomListSortBy::Name, SortDirection::Asc) => "r.name ASC, r.id ASC",
            (MyRoomListSortBy::Name, SortDirection::Desc) => "r.name DESC, r.id DESC",
            (MyRoomListSortBy::CreatedAt, SortDirection::Asc) => "r.created_at ASC, r.id ASC",
            (MyRoomListSortBy::CreatedAt, SortDirection::Desc) => "r.created_at DESC, r.id DESC",
            (MyRoomListSortBy::UpdatedAt, SortDirection::Asc) => "r.updated_at ASC, r.id ASC",
            (MyRoomListSortBy::UpdatedAt, SortDirection::Desc) => "r.updated_at DESC, r.id DESC",
            (MyRoomListSortBy::LastActivityAt, SortDirection::Asc) => {
                "r.last_activity_at ASC NULLS LAST, r.id ASC"
            }
            (MyRoomListSortBy::LastActivityAt, SortDirection::Desc) => {
                "r.last_activity_at DESC NULLS LAST, r.id DESC"
            }
        }
    }

    fn my_room_result_from_row(
        row: MyRoomListRow,
    ) -> (crate::models::Room, RoomRole, MemberStatus, i32) {
        let status = if row.closed_at.is_some() {
            crate::models::RoomStatus::Closed
        } else {
            crate::models::RoomStatus::Active
        };
        let room = crate::models::Room {
            id: row.id,
            name: row.name,
            description: row.description,
            cover_file_reference_id: None,
            created_by: row.created_by,
            status,
            is_banned: row.is_banned,
            closed_at: row.closed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            last_activity_at: row.last_activity_at,
            deleted_at: row.deleted_at,
            version: row.version,
        };

        (room, row.user_role, MemberStatus::Active, row.member_count)
    }

    /// Add user to room with role.
    ///
    pub async fn add(&self, member: &RoomMember) -> Result<RoomMember> {
        let mut tx = self.pool.begin().await?;
        let added = self.add_with_executor(member, &mut tx).await?;
        tx.commit().await?;
        Ok(added)
    }

    /// Add user to room using a provided transaction connection.
    ///
    /// Accepts `&mut PgConnection` so callers can keep room lifecycle mutations
    /// in the same transaction as adjacent room/user writes.
    pub async fn add_with_executor(
        &self,
        member: &RoomMember,
        conn: &mut sqlx::PgConnection,
    ) -> Result<RoomMember> {
        let result = sqlx::query_as!(
            RoomMemberRow,
            r#"INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                joined_at, version
             )
             SELECT $1, $2, $3, $4, $5, $6,
                    COALESCE((
                        SELECT version + 1
                        FROM room_member_versions
                        WHERE room_id = $1 AND user_id = $2
                        FOR UPDATE
                    ), 1)
             WHERE NOT EXISTS (
                SELECT 1 FROM room_member_kick_cooldowns rmkc
                WHERE rmkc.room_id = $1
                  AND rmkc.user_id = $2
                  AND rmkc.ends_at > CURRENT_TIMESTAMP
             )
             ON CONFLICT (room_id, user_id) DO NOTHING
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            member.room_id as RoomId,
            member.user_id as UserId,
            member.role as RoomRole,
            permission_bits_to_i64(member.added_permissions)?,
            permission_bits_to_i64(member.removed_permissions)?,
            member.joined_at,
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match result {
            Some(m) => {
                self.mark_active_lifecycle_with_executor(
                    &m.room_id, &m.user_id, m.version, &mut *conn,
                )
                .await?;
                Ok(m)
            }
            None => {
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
                 WHERE room_id = $1 AND user_id = $2
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

                let count = required_count(
                    sqlx::query_scalar!(
                        "SELECT COUNT(*) as count FROM (
                        SELECT 1 FROM room_members
                        WHERE room_id = $1
                        FOR UPDATE
                    ) sub",
                        member.room_id as RoomId,
                    )
                    .fetch_one(&mut **tx)
                    .await?,
                    "locked room member count",
                )?;
                let current_count = count_i64_to_u64(count, "room member count")?;
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

        // 5. Insert the new member. The lifecycle row is locked/read inside the
        // INSERT and only advanced after the insert succeeds, so failed adds do
        // not burn permission-fence versions in a caller-owned transaction.
        let result = sqlx::query_as!(
            RoomMemberRow,
            r#"INSERT INTO room_members (
                room_id, user_id, role,
                added_permissions, removed_permissions,
                joined_at, version
             )
             SELECT $1, $2, $3, $4, $5, $6,
                    COALESCE((
                        SELECT version + 1
                        FROM room_member_versions
                        WHERE room_id = $1 AND user_id = $2
                        FOR UPDATE
                    ), 1)
             WHERE NOT EXISTS (
                SELECT 1 FROM room_member_kick_cooldowns rmkc
                WHERE rmkc.room_id = $1
                  AND rmkc.user_id = $2
                  AND rmkc.ends_at > CURRENT_TIMESTAMP
             )
             ON CONFLICT (room_id, user_id) DO NOTHING
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            member.room_id as RoomId,
            member.user_id as UserId,
            member.role as RoomRole,
            permission_bits_to_i64(member.added_permissions)?,
            permission_bits_to_i64(member.removed_permissions)?,
            member.joined_at,
        )
        .fetch_optional(&mut **tx)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match result {
            Some(m) => {
                self.mark_active_lifecycle_with_executor(
                    &m.room_id, &m.user_id, m.version, &mut **tx,
                )
                .await?;
                Ok(m)
            }
            None => {
                self.diagnose_add_conflict(&member.room_id, &member.user_id, &mut **tx)
                    .await
            }
        }
    }

    /// Delete a user's current memberships from all rooms.
    ///
    /// Used during user deletion/ban to clean up room memberships.
    /// Returns the number of memberships removed.
    pub async fn remove_all_for_user(&self, user_id: &UserId) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let removed = self
            .remove_all_for_user_with_executor(user_id, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(removed.len() as u64)
    }

    /// Remove a user from all rooms using a provided executor (pool or transaction).
    ///
    /// Used to keep user lifecycle mutations and membership cleanup in the same
    /// database transaction.
    pub async fn remove_all_for_user_with_executor(
        &self,
        user_id: &UserId,
        executor: &mut sqlx::PgConnection,
    ) -> Result<Vec<RemovedRoomMember>> {
        let mut removed: Vec<RemovedRoomMember> = sqlx::query!(
            r#"DELETE FROM room_members
             WHERE user_id = $1
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       version"#,
            user_id as &UserId,
        )
        .fetch_all(&mut *executor)
        .await?
        .into_iter()
        .map(|row| RemovedRoomMember {
            room_id: row.room_id,
            user_id: row.user_id,
            version: row.version,
        })
        .collect();
        for member in &mut removed {
            member.version = self
                .mark_removed_lifecycle_with_executor(
                    &member.room_id,
                    &member.user_id,
                    member.version,
                    &mut *executor,
                )
                .await?;
        }
        Ok(removed)
    }

    /// Remove all active members from the provided rooms.
    ///
    /// Returns the memberships removed with their post-delete lifecycle versions.
    pub async fn remove_all_for_rooms_with_executor(
        &self,
        room_ids: &[RoomId],
        executor: &mut sqlx::PgConnection,
    ) -> Result<Vec<RemovedRoomMember>> {
        if room_ids.is_empty() {
            return Ok(Vec::new());
        }

        let room_id_strs: Vec<i64> = room_ids.iter().map(RoomId::as_i64).collect();
        let mut removed: Vec<RemovedRoomMember> = sqlx::query!(
            r#"DELETE FROM room_members
             WHERE room_id = ANY($1)
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       version"#,
            &room_id_strs,
        )
        .fetch_all(&mut *executor)
        .await?
        .into_iter()
        .map(|row| RemovedRoomMember {
            room_id: row.room_id,
            user_id: row.user_id,
            version: row.version,
        })
        .collect();
        for member in &mut removed {
            member.version = self
                .mark_removed_lifecycle_with_executor(
                    &member.room_id,
                    &member.user_id,
                    member.version,
                    &mut *executor,
                )
                .await?;
        }
        Ok(removed)
    }

    pub async fn mark_removed_lifecycle_with_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        removed_member_version: i64,
        executor: &mut sqlx::PgConnection,
    ) -> Result<i64> {
        let version = sqlx::query_scalar!(
            "INSERT INTO room_member_versions (room_id, user_id, version, is_member, updated_at)
             VALUES ($1, $2, $3::BIGINT + 1, FALSE, CURRENT_TIMESTAMP)
             ON CONFLICT (room_id, user_id) DO UPDATE
             SET version = GREATEST(room_member_versions.version + 1, EXCLUDED.version),
                 is_member = FALSE,
                 updated_at = CURRENT_TIMESTAMP
             RETURNING version",
            room_id as &RoomId,
            user_id as &UserId,
            removed_member_version,
        )
        .fetch_one(executor)
        .await?;
        Ok(version)
    }

    pub async fn remove_with_version_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: &mut sqlx::PgConnection,
    ) -> Result<Option<i64>> {
        let Some(row) = sqlx::query!(
            r#"DELETE FROM room_members
             WHERE room_id = $1 AND user_id = $2
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(&mut *executor)
        .await?
        else {
            return Ok(None);
        };
        let removed = RemovedRoomMember {
            room_id: row.room_id,
            user_id: row.user_id,
            version: row.version,
        };

        let version = self
            .mark_removed_lifecycle_with_executor(
                &removed.room_id,
                &removed.user_id,
                removed.version,
                executor,
            )
            .await?;
        Ok(Some(version))
    }

    /// Delete a user's current room membership.
    pub async fn remove(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let removed = self
            .remove_with_version_executor(room_id, user_id, &mut tx)
            .await?
            .is_some();
        tx.commit().await?;
        Ok(removed)
    }

    /// Get member by room and user
    pub async fn get(&self, room_id: &RoomId, user_id: &UserId) -> Result<Option<RoomMember>> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"SELECT rm.room_id as "room_id: RoomId",
                      rm.user_id as "user_id: UserId",
                      rm.role as "role: RoomRole",
                      rm.added_permissions,
                      rm.removed_permissions,
                      rm.admin_added_permissions,
                      rm.admin_removed_permissions,
                      rm.joined_at,
                      rm.version
               FROM room_members rm
               WHERE rm.room_id = $1 AND rm.user_id = $2
                 AND NOT EXISTS (
                     SELECT 1 FROM room_member_kick_cooldowns rmkc
                     WHERE rmkc.room_id = rm.room_id
                       AND rmkc.user_id = rm.user_id
                       AND rmkc.ends_at > CURRENT_TIMESTAMP
                 )"#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        Ok(member)
    }

    /// Get current member by ID without applying kick cooldown access filters.
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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"SELECT rm.room_id as "room_id: RoomId",
                      rm.user_id as "user_id: UserId",
                      rm.role as "role: RoomRole",
                      rm.added_permissions,
                      rm.removed_permissions,
                      rm.admin_added_permissions,
                      rm.admin_removed_permissions,
                      rm.joined_at,
                      rm.version
               FROM room_members rm
               WHERE rm.room_id = $1 AND rm.user_id = $2"#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
                rm.joined_at,
                TRUE AS "is_active!",
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1
	               AND NOT EXISTS (
	                   SELECT 1 FROM room_member_kick_cooldowns rmkc
	                   WHERE rmkc.room_id = rm.room_id
	                     AND rmkc.user_id = rm.user_id
	                     AND rmkc.ends_at > CURRENT_TIMESTAMP
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
    /// Uses a separate count query so out-of-range pages still return the
    /// number of matching members.
    ///
    /// Returns a tuple of (members, total_count) where:
    /// - `members`: Vec of `RoomMemberWithUser` for the current page
    /// - `total_count`: Total number of active members in the room (i64)
    ///
    /// # Exclusions
    ///
    /// This method excludes members blocked by active kick cooldown rules and
    /// soft-deleted users.
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
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));

        let mut count_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT COUNT(*) FROM room_members rm JOIN users u ON rm.user_id = u.id WHERE rm.room_id = ",
        );
        count_builder.push_bind(room_id);
        count_builder.push(" AND ");
        count_builder.push(ACTIVE_KICK_COOLDOWN_NOT_EXISTS_SQL);
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

        let mut list_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT
                rm.room_id, rm.user_id, rm.role,
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at,
                TRUE AS is_active,
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = ",
        );
        list_builder.push_bind(room_id);
        list_builder.push(" AND ");
        list_builder.push(ACTIVE_KICK_COOLDOWN_NOT_EXISTS_SQL);
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
        Self::push_room_member_order_by(&mut list_builder, query);
        list_builder.push(" LIMIT ");
        list_builder.push_bind(limit);
        list_builder.push(" OFFSET ");
        list_builder.push_bind(offset);

        let rows = list_builder
            .build_query_as::<RoomMemberWithUserRow>()
            .fetch_all(&self.pool)
            .await?;
        let members: Result<Vec<RoomMemberWithUser>> = rows
            .into_iter()
            .map(Self::typed_row_to_member_with_user)
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
                rm.joined_at,
                TRUE AS "is_active!",
                u.username
             FROM room_members rm
             JOIN users u ON rm.user_id = u.id
             WHERE rm.room_id = $1
	               AND NOT EXISTS (
	                   SELECT 1 FROM room_member_kick_cooldowns rmkc
	                   WHERE rmkc.room_id = rm.room_id
	                     AND rmkc.user_id = rm.user_id
	                     AND rmkc.ends_at > CURRENT_TIMESTAMP
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

    /// Update member role with optimistic locking.
    pub async fn update_role(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET role = $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            role as RoomRole,
            current_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn update_role_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        self.update_role_with_exact_version_executor(
            room_id,
            user_id,
            role,
            current_version,
            new_version,
            &self.pool,
        )
        .await
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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET role = $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            role as RoomRole,
            current_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn update_role_with_exact_version_executor(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
        executor: impl sqlx::PgExecutor<'_>,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET role = $3,
                 version = $5
             WHERE room_id = $1 AND user_id = $2 AND version = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            role as RoomRole,
            current_version,
            new_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn update_role_and_permissions_with_exact_version_executor<'e, E>(
        &self,
        update: MemberRolePermissionExactVersionUpdate<'_>,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if update.new_version <= update.current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {} must be greater than current version {}",
                update.new_version, update.current_version
            )));
        }

        let (
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
        ) = if update.use_admin_permissions {
            (
                0,
                0,
                permission_bits_to_i64(update.added_permissions)?,
                permission_bits_to_i64(update.removed_permissions)?,
            )
        } else {
            (
                permission_bits_to_i64(update.added_permissions)?,
                permission_bits_to_i64(update.removed_permissions)?,
                0,
                0,
            )
        };

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET
                role = $3,
                added_permissions = $4,
                removed_permissions = $5,
                admin_added_permissions = $6,
                admin_removed_permissions = $7,
                version = $9
             WHERE room_id = $1 AND user_id = $2 AND version = $8
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            update.room_id as &RoomId,
            update.user_id as &UserId,
            update.role as RoomRole,
            added_permissions,
            removed_permissions,
            admin_added_permissions,
            admin_removed_permissions,
            update.current_version,
            update.new_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update member Allow/Deny permissions with optimistic locking.
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

    pub async fn update_permissions_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        self.update_permissions_with_exact_version_executor(
            MemberPermissionExactVersionUpdate {
                room_id,
                user_id,
                added_permissions,
                removed_permissions,
                current_version,
                new_version,
            },
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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = $3,
                 removed_permissions = $4,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(added_permissions)?,
            permission_bits_to_i64(removed_permissions)?,
            current_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn update_permissions_with_exact_version_executor<'e, E>(
        &self,
        update: MemberPermissionExactVersionUpdate<'_>,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if update.new_version <= update.current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {} must be greater than current version {}",
                update.new_version, update.current_version
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = $3,
                 removed_permissions = $4,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            update.room_id as &RoomId,
            update.user_id as &UserId,
            permission_bits_to_i64(update.added_permissions)?,
            permission_bits_to_i64(update.removed_permissions)?,
            update.current_version,
            update.new_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Update admin-specific Allow/Deny permissions with optimistic locking.
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

    pub async fn update_admin_permissions_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        added_permissions: u64,
        removed_permissions: u64,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        self.update_admin_permissions_with_exact_version_executor(
            MemberPermissionExactVersionUpdate {
                room_id,
                user_id,
                added_permissions,
                removed_permissions,
                current_version,
                new_version,
            },
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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_added_permissions = $3,
                 admin_removed_permissions = $4,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(added_permissions)?,
            permission_bits_to_i64(removed_permissions)?,
            current_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn update_admin_permissions_with_exact_version_executor<'e, E>(
        &self,
        update: MemberPermissionExactVersionUpdate<'_>,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        if update.new_version <= update.current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {} must be greater than current version {}",
                update.new_version, update.current_version
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_added_permissions = $3,
                 admin_removed_permissions = $4,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            update.room_id as &RoomId,
            update.user_id as &UserId,
            permission_bits_to_i64(update.added_permissions)?,
            permission_bits_to_i64(update.removed_permissions)?,
            update.current_version,
            update.new_version,
        )
        .fetch_optional(executor)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically grant permission bits (bitwise OR in SQL to avoid read-modify-write TOCTOU)
    ///
    /// Only applies to current members. Returns `NotFound` if the membership no longer exists.
    pub async fn grant_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = added_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = added_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND role = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn grant_permission_atomic_for_role_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = added_permissions | $3,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND role = $4 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
            current_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically grant admin-specific permission bits.
    ///
    /// Only applies to current members. Returns `NotFound` if the membership no longer exists.
    pub async fn grant_admin_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_added_permissions = admin_added_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_added_permissions = admin_added_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND role = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn grant_admin_permission_atomic_for_role_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_added_permissions = admin_added_permissions | $3,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND role = $4 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
            current_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically revoke permission bits (bitwise OR on `removed_permissions` in SQL)
    ///
    /// Only applies to current members. Returns `NotFound` if the membership no longer exists.
    pub async fn revoke_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET removed_permissions = removed_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET removed_permissions = removed_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND role = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn revoke_permission_atomic_for_role_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET removed_permissions = removed_permissions | $3,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND role = $4 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
            current_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Atomically revoke admin-specific permission bits.
    ///
    /// Only applies to current members. Returns `NotFound` if the membership no longer exists.
    pub async fn revoke_admin_permission_atomic(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
    ) -> Result<RoomMember> {
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_removed_permissions = admin_removed_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_removed_permissions = admin_removed_permissions | $3,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND role = $4
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn revoke_admin_permission_atomic_for_role_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        permission: u64,
        role: RoomRole,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET admin_removed_permissions = admin_removed_permissions | $3,
                 version = $6
             WHERE room_id = $1 AND user_id = $2 AND role = $4 AND version = $5
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            permission_bits_to_i64(permission)?,
            role as RoomRole,
            current_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

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
        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = 0,
                 removed_permissions = 0,
                 admin_added_permissions = 0,
                 admin_removed_permissions = 0,
                 version = version + 1
             WHERE room_id = $1 AND user_id = $2 AND version = $3
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            current_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    pub async fn reset_permissions_with_exact_version(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        current_version: i64,
        new_version: i64,
    ) -> Result<RoomMember> {
        if new_version <= current_version {
            return Err(Error::InvalidInput(format!(
                "new member version {new_version} must be greater than current version {current_version}"
            )));
        }

        let member = sqlx::query_as!(
            RoomMemberRow,
            r#"UPDATE room_members
             SET added_permissions = 0,
                 removed_permissions = 0,
                 admin_added_permissions = 0,
                 admin_removed_permissions = 0,
                 version = $4
             WHERE room_id = $1 AND user_id = $2 AND version = $3
             RETURNING room_id as "room_id: RoomId",
                       user_id as "user_id: UserId",
                       role as "role: RoomRole",
                       added_permissions,
                       removed_permissions,
                       admin_added_permissions,
                       admin_removed_permissions,
                       joined_at,
                       version"#,
            room_id as &RoomId,
            user_id as &UserId,
            current_version,
            new_version,
        )
        .fetch_optional(&self.pool)
        .await?
        .map(Self::typed_row_to_member)
        .transpose()?;

        match member {
            Some(m) => Ok(m),
            None => Err(Error::OptimisticLockConflict),
        }
    }

    /// Check if user is an active member of room.
    pub async fn is_member(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_members rm
                WHERE rm.room_id = $1 AND rm.user_id = $2
                  AND NOT EXISTS (
                      SELECT 1
                      FROM room_member_kick_cooldowns rmkc
                      WHERE rmkc.room_id = rm.room_id
                        AND rmkc.user_id = rm.user_id
                        AND rmkc.ends_at > CURRENT_TIMESTAMP
                  )
            ) as "exists!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    pub async fn is_in_kick_cooldown(&self, room_id: &RoomId, user_id: &UserId) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_member_kick_cooldowns
                WHERE room_id = $1
                  AND user_id = $2
                  AND ends_at > CURRENT_TIMESTAMP
            ) as "exists!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(exists)
    }

    pub async fn is_in_kick_cooldown_with_executor<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM room_member_kick_cooldowns
                WHERE room_id = $1
                  AND user_id = $2
                  AND ends_at > CURRENT_TIMESTAMP
            ) as "exists!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(executor)
        .await?;

        Ok(exists)
    }

    pub async fn add_kick_cooldown_with_executor<'e, E>(
        &self,
        insert: KickCooldownInsert<'_>,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        sqlx::query!(
            r#"
            INSERT INTO room_member_kick_cooldowns (
                room_id, user_id, kicked_by, starts_at, ends_at, reason
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
            insert.room_id as &RoomId,
            insert.user_id as &UserId,
            insert.kicked_by.map(UserId::as_i64),
            insert.starts_at,
            insert.ends_at,
            insert.reason,
        )
        .execute(executor)
        .await?;

        Ok(())
    }

    /// Get member count for room
    pub async fn count_by_room(&self, room_id: &RoomId) -> Result<i32> {
        let count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) as "count!"
            FROM room_members rm
            WHERE rm.room_id = $1
             
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_kick_cooldowns rmkc
                  WHERE rmkc.room_id = rm.room_id
                    AND rmkc.user_id = rm.user_id
                    AND rmkc.ends_at > CURRENT_TIMESTAMP
              )
            "#,
            room_id as &RoomId,
        )
        .fetch_one(&self.pool)
        .await?;

        i32::try_from(count)
            .map_err(|_| Error::Internal("room member count exceeds i32::MAX".to_string()))
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
             
              AND NOT EXISTS (
                  SELECT 1 FROM room_member_kick_cooldowns rmkc
                  WHERE rmkc.room_id = rm.room_id
                    AND rmkc.user_id = rm.user_id
                    AND rmkc.ends_at > CURRENT_TIMESTAMP
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
        let limit = pagination.limit_i64()?;
        let offset = pagination.offset_i64()?;

        let total_count = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*)
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            WHERE rm.user_id = $1 AND r.deleted_at IS NULL
            "#,
            user_id as &UserId,
        )
        .fetch_one(&self.pool)
        .await?;
        let total_count = required_count(total_count, "user room membership total")?;

        let rows = sqlx::query!(
            r#"
            SELECT rm.room_id as "room_id: RoomId"
             FROM room_members rm
             JOIN rooms r ON rm.room_id = r.id
             WHERE rm.user_id = $1 AND r.deleted_at IS NULL
             ORDER BY rm.joined_at DESC
             LIMIT $2 OFFSET $3
            "#,
            user_id as &UserId,
            limit,
            offset,
        )
        .fetch_all(&self.pool)
        .await?;

        let room_ids = rows.into_iter().map(|row| row.room_id).collect();

        Ok((room_ids, total_count))
    }

    /// Get rooms where a user is a member with full room details and member count (optimized)
    /// Returns (room, role, status, `member_count`) tuples
    ///
    /// Uses a separate count query so out-of-range pages still return the
    /// number of matching rooms.
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
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (count_where_sql, _) = wb.build(2)?;
        let (where_sql, _) = wb.build(4)?;
        let order_by_sql = Self::my_room_order_by_sql(query);
        let count_sql = format!(
            r"
            SELECT COUNT(*)
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            WHERE rm.user_id = $1 AND {count_where_sql}
            "
        );
        let total_count = Self::bind_my_room_count_filters(
            sqlx::query_scalar::<_, i64>(trusted_dynamic_sql(count_sql)).bind(user_id),
            search_pattern.as_deref(),
        )
        .fetch_one(&self.pool)
        .await?;
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.closed_at,
                r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                {ACTIVE_ROOM_BAN_EXISTS_SQL} AS is_banned,
                rm.role as user_role,
                COUNT(rm2.user_id)::int as member_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2
                ON r.id = rm2.room_id
	                   AND NOT EXISTS (
	                       SELECT 1 FROM room_member_kick_cooldowns rmkc2
	                       WHERE rmkc2.room_id = rm2.room_id
	                         AND rmkc2.user_id = rm2.user_id
	                         AND rmkc2.ends_at > CURRENT_TIMESTAMP
	                   )
		            WHERE rm.user_id = $1 AND {where_sql}
	            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at,
	                     r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
	                     rm.role, rm.joined_at
            ORDER BY {order_by_sql}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query_as::<_, MyRoomListRow>(trusted_dynamic_sql(sql))
                .bind(user_id)
                .bind(limit)
                .bind(offset),
            search_pattern.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;

        let results = rows
            .into_iter()
            .map(Self::my_room_result_from_row)
            .collect();

        Ok((results, total_count))
    }

    /// List only rooms whose creator is still active.
    pub async fn list_accessible_by_user_with_query(
        &self,
        user_id: &UserId,
        query: &MyRoomListQuery,
    ) -> Result<(Vec<(crate::models::Room, RoomRole, MemberStatus, i32)>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|value| escape_ilike(value));
        let wb = Self::build_my_room_list_conditions(query);
        let (count_where_sql, _) = wb.build(2)?;
        let (where_sql, _) = wb.build(4)?;
        let order_by_sql = Self::my_room_order_by_sql(query);
        let count_sql = format!(
            r"
            SELECT COUNT(*)
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            WHERE rm.user_id = $1 AND {count_where_sql} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}
            "
        );
        let total_count = Self::bind_my_room_count_filters(
            sqlx::query_scalar::<_, i64>(trusted_dynamic_sql(count_sql)).bind(user_id),
            search_pattern.as_deref(),
        )
        .fetch_one(&self.pool)
        .await?;
        let sql = format!(
            r"
            SELECT
                r.id, r.name, r.description, r.created_by, r.closed_at,
                r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
                {ACTIVE_ROOM_BAN_EXISTS_SQL} AS is_banned,
                rm.role as user_role,
                COUNT(rm2.user_id)::int as member_count
            FROM room_members rm
            JOIN rooms r ON rm.room_id = r.id
            LEFT JOIN room_members rm2
                ON r.id = rm2.room_id
	                   AND NOT EXISTS (
	                       SELECT 1 FROM room_member_kick_cooldowns rmkc2
	                       WHERE rmkc2.room_id = rm2.room_id
	                         AND rmkc2.user_id = rm2.user_id
	                         AND rmkc2.ends_at > CURRENT_TIMESTAMP
	                   )
	            WHERE rm.user_id = $1 AND {where_sql} AND {ACCESSIBLE_ROOM_CREATOR_CONDITION}
	            GROUP BY r.id, r.name, r.description, r.created_by, r.closed_at,
	                     r.created_at, r.updated_at, r.deleted_at, r.version, r.last_activity_at,
	                     rm.role, rm.joined_at
            ORDER BY {order_by_sql}
            LIMIT $2 OFFSET $3
            "
        );

        let rows = Self::bind_my_room_filters(
            sqlx::query_as::<_, MyRoomListRow>(trusted_dynamic_sql(sql))
                .bind(user_id)
                .bind(limit)
                .bind(offset),
            search_pattern.as_deref(),
        )
        .fetch_all(&self.pool)
        .await?;

        let results = rows
            .into_iter()
            .map(Self::my_room_result_from_row)
            .collect();

        Ok((results, total_count))
    }

    /// List all current members for an admin view.
    pub async fn list_by_room_all(&self, room_id: &RoomId) -> Result<Vec<RoomMemberWithUser>> {
        let rows = sqlx::query_as!(
            RoomMemberWithUserRow,
            r#"SELECT
                rm.room_id AS "room_id: RoomId",
                rm.user_id AS "user_id: UserId",
                rm.role AS "role: RoomRole",
                rm.added_permissions, rm.removed_permissions,
                rm.admin_added_permissions, rm.admin_removed_permissions,
                rm.joined_at,
                u.username,
                TRUE AS "is_active!"
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

    /// Atomically kick a member only if the actor has a strictly higher role.
    ///
    /// Role values in DB: 1=Creator, 2=Admin, 3=Member, 4=Guest (lower = higher authority).
    /// The WHERE clause `actor.role < target.role` ensures the actor outranks the target,
    /// eliminating the TOCTOU race between checking roles and deleting the
    /// target membership row.
    ///
    /// Returns `Ok(true)` if the member was kicked, `Ok(false)` if the target was not
    /// found or the actor does not outrank the target.
    pub async fn kick_with_role_check(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let removed = self
            .kick_with_role_check_with_executor(room_id, actor_id, target_id, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(removed.is_some())
    }

    pub async fn kick_with_role_check_with_executor(
        &self,
        room_id: &RoomId,
        actor_id: &UserId,
        target_id: &UserId,
        executor: &mut sqlx::PgConnection,
    ) -> Result<Option<i64>> {
        let Some(row) = sqlx::query!(
            r#"DELETE FROM room_members AS target
             WHERE target.room_id = $1
               AND target.user_id = $3
               AND EXISTS (
                   SELECT 1 FROM room_members AS actor
                   WHERE actor.room_id = $1
                     AND actor.user_id = $2
                     AND actor.role < target.role
             )
             RETURNING target.room_id as "room_id: RoomId",
                       target.user_id as "user_id: UserId",
                       target.version"#,
            room_id as &RoomId,
            actor_id as &UserId,
            target_id as &UserId,
        )
        .fetch_optional(&mut *executor)
        .await?
        else {
            return Ok(None);
        };
        let removed = RemovedRoomMember {
            room_id: row.room_id,
            user_id: row.user_id,
            version: row.version,
        };

        self.mark_removed_lifecycle_with_executor(
            &removed.room_id,
            &removed.user_id,
            removed.version,
            executor,
        )
        .await
        .map(Some)
    }

    /// Diagnose why a guarded insert did not return a membership row.
    async fn diagnose_add_conflict<'e, E>(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        executor: E,
    ) -> Result<RoomMember>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query!(
            r#"
            SELECT
                EXISTS (
                    SELECT 1 FROM room_members
                    WHERE room_id = $1 AND user_id = $2
                ) AS "is_member!",
                EXISTS (
                    SELECT 1 FROM room_member_kick_cooldowns
                    WHERE room_id = $1 AND user_id = $2
                      AND ends_at > CURRENT_TIMESTAMP
                ) AS "is_in_kick_cooldown!"
            "#,
            room_id as &RoomId,
            user_id as &UserId,
        )
        .fetch_one(executor)
        .await?;

        if row.is_in_kick_cooldown {
            return Err(Error::Authorization(
                KICK_COOLDOWN_DENIED_MESSAGE.to_string(),
            ));
        }
        if row.is_member {
            return Err(Error::AlreadyExists(
                "Already a member of this room".to_string(),
            ));
        }

        Err(Error::Internal(
            "Unexpected conflict adding room member".to_string(),
        ))
    }

    /// Convert database row to `RoomMemberWithUser`
    fn typed_row_to_member_with_user(row: RoomMemberWithUserRow) -> Result<RoomMemberWithUser> {
        Ok(RoomMemberWithUser {
            room_id: row.room_id,
            user_id: row.user_id,
            username: row.username,
            role: row.role,
            status: MemberStatus::Active,
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
            is_online: false,
            is_active: row.is_active,
        })
    }

    fn typed_row_to_member(row: RoomMemberRow) -> Result<RoomMember> {
        Ok(RoomMember {
            room_id: row.room_id,
            user_id: row.user_id,
            role: row.role,
            status: MemberStatus::Active,
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
            version: row.version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SortDirection;

    fn room_member_order_by_sql(query: &RoomMemberListQuery) -> String {
        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("");
        RoomMemberRepository::push_room_member_order_by(&mut builder, query);
        builder.sql().as_str().to_string()
    }

    #[test]
    fn room_member_order_by_uses_static_sort_branches() {
        let mut query = RoomMemberListQuery {
            sort_by: RoomMemberListSortBy::Username,
            sort_direction: SortDirection::Desc,
            ..RoomMemberListQuery::default()
        };
        assert_eq!(
            room_member_order_by_sql(&query),
            " ORDER BY u.username DESC, rm.user_id ASC"
        );

        query.sort_by = RoomMemberListSortBy::Role;
        query.sort_direction = SortDirection::Asc;
        assert_eq!(
            room_member_order_by_sql(&query),
            " ORDER BY rm.role ASC, rm.joined_at ASC, rm.user_id ASC"
        );
    }

    #[test]
    fn my_room_order_by_uses_static_sort_branches() {
        let mut query = MyRoomListQuery {
            sort_by: MyRoomListSortBy::LastActivityAt,
            sort_direction: SortDirection::Desc,
            ..MyRoomListQuery::default()
        };
        assert_eq!(
            RoomMemberRepository::my_room_order_by_sql(&query),
            "r.last_activity_at DESC NULLS LAST, r.id DESC"
        );

        query.sort_by = MyRoomListSortBy::Name;
        query.sort_direction = SortDirection::Asc;
        assert_eq!(
            RoomMemberRepository::my_room_order_by_sql(&query),
            "r.name ASC, r.id ASC"
        );
    }
}
