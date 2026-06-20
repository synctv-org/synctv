use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use super::query_builder::escape_ilike;
use crate::repository::pools::RepoPools;
use crate::{
    models::{SignupMethod, User, UserId, UserListQuery, UserListSortBy, UserRole, UserStatus},
    Error, Result,
};

const ACTIVE_USER_BAN_EXISTS_SQL: &str = "u.is_banned = TRUE";
const ACTIVE_USER_BAN_NOT_EXISTS_SQL: &str = "u.is_banned = FALSE";

#[derive(Clone, Copy)]
enum UserListRoleScope {
    All,
    Admins,
}

#[derive(sqlx::FromRow)]
struct UserListRow {
    id: UserId,
    username: String,
    signup_method: SignupMethod,
    role: UserRole,
    avatar_file_reference_id: Option<i64>,
    status: UserStatus,
    is_banned: bool,
    banned_at: Option<DateTime<Utc>>,
    banned_by: Option<UserId>,
    banned_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    version: i32,
    deleted_at: Option<DateTime<Utc>>,
}

impl From<UserListRow> for User {
    fn from(row: UserListRow) -> Self {
        Self {
            id: row.id,
            username: row.username,
            role: row.role,
            avatar_file_reference_id: row.avatar_file_reference_id,
            status: row.status,
            is_banned: row.is_banned,
            banned_at: row.banned_at,
            banned_by: row.banned_by,
            banned_reason: row.banned_reason,
            signup_method: row.signup_method,
            created_at: row.created_at,
            updated_at: row.updated_at,
            version: row.version,
            deleted_at: row.deleted_at,
        }
    }
}

/// User repository for database operations
#[derive(Clone)]
pub struct UserRepository {
    pools: RepoPools,
}

impl UserRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
        }
    }

    /// Get the database pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.pools.primary()
    }

    #[must_use]
    pub fn eventually_consistent_pool(&self) -> &PgPool {
        self.pools.read()
    }

    /// Create a new user.
    pub async fn create(&self, user: &User) -> Result<User> {
        self.create_with_executor(user, self.pool()).await
    }

    /// Create a new user using a provided executor.
    pub async fn create_with_executor<'e, E>(&self, user: &User, executor: E) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as!(
            User,
            r#"
            WITH inserted_user AS (
                INSERT INTO users (username, signup_method, role, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5)
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            )
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM inserted_user p
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = p.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            &user.username,
            i16::from(user.signup_method),
            i16::from(user.role),
            user.created_at,
            user.updated_at
        )
        .fetch_one(executor)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                let constraint = db_err.constraint().unwrap_or("");
                if constraint.contains("username") {
                    Error::AlreadyExists("Username already taken".to_string())
                } else {
                    Error::AlreadyExists(
                        synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                    )
                }
            }
            _ => Error::Database(e),
        })?;

        Ok(u)
    }

    /// Get user by ID
    pub async fn get_by_id(&self, user_id: &UserId) -> Result<Option<User>> {
        let u = sqlx::query_as!(
            User,
            r#"
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   p.status AS "status!: UserStatus",
                   p.is_banned AS "is_banned!",
                   p.banned_at,
                   p.banned_by AS "banned_by?: UserId",
                   p.banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM user_account_profiles p
            WHERE p.id = $1 AND p.deleted_at IS NULL
            "#,
            user_id.as_i64()
        )
        .fetch_optional(self.pool())
        .await?;

        Ok(u)
    }

    /// Get user by ID using a provided executor and lock the row for update.
    pub async fn get_by_id_for_update_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        executor: E,
    ) -> Result<Option<User>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as!(
            User,
            r#"
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   p.status AS "status!: UserStatus",
                   p.is_banned AS "is_banned!",
                   p.banned_at,
                   p.banned_by AS "banned_by?: UserId",
                   p.banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM user_account_profiles p
            JOIN users u ON u.id = p.id
            WHERE p.id = $1 AND p.deleted_at IS NULL
            FOR UPDATE OF u
            "#,
            user_id.as_i64()
        )
        .fetch_optional(executor)
        .await?;

        Ok(u)
    }

    /// Get multiple users by IDs in a single batch query
    pub async fn get_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = user_ids
            .iter()
            .map(super::super::models::id::UserId::as_i64)
            .collect();
        let users = sqlx::query_as!(
            User,
            r#"
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   p.status AS "status!: UserStatus",
                   p.is_banned AS "is_banned!",
                   p.banned_at,
                   p.banned_by AS "banned_by?: UserId",
                   p.banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM user_account_profiles p
            WHERE p.id = ANY($1) AND p.deleted_at IS NULL
            "#,
            &ids
        )
        .fetch_all(self.pool())
        .await?;

        Ok(users)
    }

    /// Get user by username
    pub async fn get_by_username(&self, username: &str) -> Result<Option<User>> {
        self.get_by_username_with_executor(username, self.pool())
            .await
    }

    pub async fn get_by_username_with_executor<'e, E>(
        &self,
        username: &str,
        executor: E,
    ) -> Result<Option<User>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as!(
            User,
            r#"
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   p.status AS "status!: UserStatus",
                   p.is_banned AS "is_banned!",
                   p.banned_at,
                   p.banned_by AS "banned_by?: UserId",
                   p.banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM user_account_profiles p
            WHERE LOWER(p.username) = LOWER($1) AND p.deleted_at IS NULL
            "#,
            username
        )
        .fetch_optional(executor)
        .await?;

        Ok(u)
    }

    /// Update user with optimistic locking.
    ///
    /// The caller must pass the `version` value from the previously-read user.
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
    pub async fn update(&self, user: &User, old_version: i32) -> Result<User> {
        self.update_with_executor(user, old_version, self.pool())
            .await
    }

    /// Update only the user's global role with optimistic locking.
    pub async fn update_role(
        &self,
        user_id: &UserId,
        role: crate::models::UserRole,
        old_version: i32,
    ) -> Result<User> {
        let u = sqlx::query_as!(
            User,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET role = $2,
                    updated_at = $3,
                    version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $4
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            )
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM updated_user p
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = p.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user_id.as_i64(),
            i16::from(role),
            Utc::now(),
            old_version
        )
        .fetch_optional(self.pool())
        .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {user_id} not found")))
            }
        }
    }

    /// Update user with optimistic locking using a provided executor.
    pub async fn update_with_executor<'e, E>(
        &self,
        user: &User,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as!(
            User,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET username = $2, role = $3,
                    updated_at = $4, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $5
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            )
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM updated_user p
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = p.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user.id.as_i64(),
            &user.username,
            i16::from(user.role),
            Utc::now(),
            old_version
        )
        .fetch_optional(executor)
        .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            // Check if the user exists at all to distinguish
            // "not found" from "concurrent modification"
            let exists = self.get_by_id(&user.id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {} not found", user.id)))
            }
        }
    }

    /// Update the user profile atomically with optimistic locking.
    ///
    /// Supports updating user profile fields with optimistic locking. Password
    /// credentials and password metadata are managed by `UserPasswordRepository`.
    pub async fn update_profile_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        username: &str,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let u = sqlx::query_as!(
            User,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET username = $2,
                    updated_at = $3,
                    version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $4
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            )
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM updated_user p
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = p.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user_id.as_i64(),
            username,
            now,
            old_version
        )
        .fetch_optional(executor)
        .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {user_id} not found")))
            }
        }
    }

    pub async fn update_avatar_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        avatar_file_reference_id: Option<i64>,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as!(
            User,
            r#"
            WITH updated_user AS (
                UPDATE users
                SET avatar_file_reference_id = $2,
                    updated_at = $3,
                    version = version + 1
                WHERE id = $1 AND deleted_at IS NULL AND version = $4
                RETURNING id, username, signup_method, role,
                          avatar_file_reference_id,
                          created_at, updated_at,
                          version, deleted_at
            )
            SELECT p.id AS "id!: UserId",
                   p.username AS "username!",
                   p.signup_method AS "signup_method!: SignupMethod",
                   p.role AS "role!: UserRole",
                   p.avatar_file_reference_id,
                   CASE
                       WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
                       ELSE 2::SMALLINT
                   END AS "status!: UserStatus",
                   (active_ban.user_id IS NOT NULL) AS "is_banned!",
                   active_ban.starts_at AS banned_at,
                   active_ban.banned_by AS "banned_by?: UserId",
                   active_ban.reason AS banned_reason,
                   p.created_at AS "created_at!",
                   p.updated_at AS "updated_at!",
                   p.version AS "version!",
                   p.deleted_at
            FROM updated_user p
            LEFT JOIN LATERAL (
                SELECT ub.user_id,
                       ub.starts_at,
                       ub.banned_by,
                       ub.reason
                FROM user_bans ub
                WHERE ub.user_id = p.id
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                ORDER BY ub.starts_at DESC
                LIMIT 1
            ) active_ban ON TRUE
            "#,
            user_id.as_i64(),
            avatar_file_reference_id,
            Utc::now(),
            old_version
        )
        .fetch_optional(executor)
        .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!("User {user_id} not found")))
            }
        }
    }

    /// Soft delete user
    pub async fn delete(&self, user_id: &UserId) -> Result<bool> {
        self.delete_with_executor(user_id, self.pool()).await
    }

    /// Soft delete user using a provided executor (pool or transaction)
    pub async fn delete_with_executor<'e, E>(&self, user_id: &UserId, executor: E) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query!(
            r"
            UPDATE users
            SET deleted_at = $2, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            ",
            user_id as &UserId,
            Utc::now(),
        )
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Globally ban a user without changing lifecycle status.
    pub async fn ban(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
    ) -> Result<User> {
        self.insert_ban_with_executor(user_id, banned_by, reason, self.pool())
            .await?;
        self.get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    /// Insert a global ban record using a provided executor.
    pub async fn insert_ban_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        banned_by: Option<&UserId>,
        reason: Option<String>,
        executor: E,
    ) -> Result<()>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let lock_key = format!("user-ban:{user_id}");
        let inserted = sqlx::query!(
            r"
            WITH _lock AS (
                SELECT pg_advisory_xact_lock(hashtextextended($5, 0))
            )
            INSERT INTO user_bans (user_id, banned_by, reason, starts_at)
            SELECT u.id, $2, $3, $4
            FROM users u, _lock
            WHERE u.id = $1 AND u.deleted_at IS NULL
              AND NOT EXISTS (
                  SELECT 1 FROM user_bans ub
                  WHERE ub.user_id = u.id
                    AND ub.revoked_at IS NULL
                    AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
                  )
            ",
            user_id.as_i64(),
            banned_by.map(UserId::as_i64),
            reason,
            now,
            lock_key
        )
        .execute(executor)
        .await?;

        if inserted.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "User {user_id} not found or already banned"
            )));
        }

        Ok(())
    }

    /// Clear a global user ban without changing lifecycle status.
    pub async fn unban(&self, user_id: &UserId) -> Result<User> {
        let result = sqlx::query!(
            r"
            UPDATE user_bans ub
            SET revoked_at = $2
            FROM users u
            WHERE ub.user_id = u.id
              AND u.id = $1
              AND u.deleted_at IS NULL
              AND ub.revoked_at IS NULL
              AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
            ",
            user_id as &UserId,
            Utc::now(),
        )
        .execute(self.pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::NotFound(format!(
                "User {user_id} not found or not banned"
            )));
        }

        let u = self
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))?;

        Ok(u)
    }

    /// Check whether a user has an active global ban.
    pub async fn is_banned(&self, user_id: &UserId) -> Result<bool> {
        let is_banned = sqlx::query_scalar!(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM user_bans ub
                JOIN users u ON u.id = ub.user_id
                WHERE u.id = $1
                  AND u.deleted_at IS NULL
                  AND ub.revoked_at IS NULL
                  AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
            ) as "exists!"
            "#,
            user_id as &UserId,
        )
        .fetch_one(self.pool())
        .await?;

        Ok(is_banned)
    }

    fn order_by_sql(query: &UserListQuery) -> &'static str {
        use crate::models::SortDirection;

        match (query.sort_by, query.sort_direction) {
            (UserListSortBy::Username, SortDirection::Asc) => "username ASC, id ASC",
            (UserListSortBy::Username, SortDirection::Desc) => "username DESC, id DESC",
            (UserListSortBy::Email, SortDirection::Asc) => "email ASC NULLS LAST, id ASC",
            (UserListSortBy::Email, SortDirection::Desc) => "email DESC NULLS LAST, id DESC",
            (UserListSortBy::Status, SortDirection::Asc) => "is_banned ASC, created_at ASC, id ASC",
            (UserListSortBy::Status, SortDirection::Desc) => {
                "is_banned DESC, created_at DESC, id DESC"
            }
            (UserListSortBy::Role, SortDirection::Asc) => "role ASC, created_at ASC, id ASC",
            (UserListSortBy::Role, SortDirection::Desc) => "role DESC, created_at DESC, id DESC",
            (UserListSortBy::UpdatedAt, SortDirection::Asc) => "updated_at ASC, id ASC",
            (UserListSortBy::UpdatedAt, SortDirection::Desc) => "updated_at DESC, id DESC",
            (UserListSortBy::CreatedAt, SortDirection::Asc) => "created_at ASC, id ASC",
            (UserListSortBy::CreatedAt, SortDirection::Desc) => "created_at DESC, id DESC",
        }
    }

    fn push_user_list_from_and_filters<'a>(
        builder: &mut QueryBuilder<Postgres>,
        query: &'a UserListQuery,
        role_scope: UserListRoleScope,
        search_pattern: Option<&'a str>,
    ) {
        builder.push(
            " FROM user_account_profiles u \
             LEFT JOIN auth_email_identities aei ON aei.user_id = u.id \
             WHERE u.deleted_at IS NULL",
        );

        if matches!(role_scope, UserListRoleScope::Admins) {
            builder
                .push(" AND u.role IN (")
                .push_bind(i16::from(UserRole::Root))
                .push(", ")
                .push_bind(i16::from(UserRole::Admin))
                .push(")");
        }
        if let Some(pattern) = search_pattern {
            builder
                .push(" AND (u.username ILIKE ")
                .push_bind(pattern)
                .push(" OR aei.email ILIKE ")
                .push_bind(pattern)
                .push(")");
        }
        if let Some(role) = query.role {
            builder.push(" AND u.role = ").push_bind(i16::from(role));
        }
        match query.status {
            Some(UserStatus::Active) => {
                builder.push(" AND ").push(ACTIVE_USER_BAN_NOT_EXISTS_SQL);
            }
            Some(UserStatus::Banned) => {
                builder.push(" AND ").push(ACTIVE_USER_BAN_EXISTS_SQL);
            }
            None => {}
        }
        match query.is_banned {
            Some(true) => {
                builder.push(" AND ").push(ACTIVE_USER_BAN_EXISTS_SQL);
            }
            Some(false) => {
                builder.push(" AND ").push(ACTIVE_USER_BAN_NOT_EXISTS_SQL);
            }
            None => {}
        }
    }

    async fn list_with_role_scope(
        &self,
        query: &UserListQuery,
        role_scope: UserListRoleScope,
        pool: &PgPool,
    ) -> Result<(Vec<User>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let mut count_builder = QueryBuilder::<Postgres>::new("SELECT COUNT(*)");
        Self::push_user_list_from_and_filters(
            &mut count_builder,
            query,
            role_scope,
            search_pattern.as_deref(),
        );
        let count: i64 = count_builder.build_query_scalar().fetch_one(pool).await?;

        let mut list_builder = QueryBuilder::<Postgres>::new(
            "SELECT \
             u.id, u.username, \
             u.signup_method, u.role, \
             u.avatar_file_reference_id, \
             u.status, \
             u.is_banned, \
             u.banned_at, \
             u.banned_by, \
             u.banned_reason, \
             u.created_at, u.updated_at, \
             u.version, u.deleted_at",
        );
        Self::push_user_list_from_and_filters(
            &mut list_builder,
            query,
            role_scope,
            search_pattern.as_deref(),
        );
        list_builder
            .push(" ORDER BY ")
            .push(Self::order_by_sql(query))
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let rows = list_builder
            .build_query_as::<UserListRow>()
            .fetch_all(pool)
            .await?;
        let users = rows.into_iter().map(User::from).collect();

        Ok((users, count))
    }

    /// List users with pagination
    pub async fn list(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        self.list_with_role_scope(query, UserListRoleScope::All, self.pool())
            .await
    }

    /// List users with pagination on the eventually consistent read pool.
    pub async fn list_eventually_consistent(
        &self,
        query: &UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        self.list_with_role_scope(
            query,
            UserListRoleScope::All,
            self.eventually_consistent_pool(),
        )
        .await
    }

    /// List admin-capable users (root + admin) with pagination.
    pub async fn list_admins(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        self.list_with_role_scope(query, UserListRoleScope::Admins, self.pool())
            .await
    }

    /// List admin-capable users on the eventually consistent read pool.
    pub async fn list_admins_eventually_consistent(
        &self,
        query: &UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        self.list_with_role_scope(
            query,
            UserListRoleScope::Admins,
            self.eventually_consistent_pool(),
        )
        .await
    }

    /// Check if username exists
    pub async fn username_exists(&self, username: &str) -> Result<bool> {
        let exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM users
                WHERE LOWER(username) = LOWER($1) AND deleted_at IS NULL
            ) AS "exists!"
            "#,
            username
        )
        .fetch_one(self.pool())
        .await?;

        Ok(exists)
    }
}

#[cfg(test)]
#[path = "user_tests.rs"]
mod tests;
