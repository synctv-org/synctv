use chrono::Utc;
use sqlx::PgPool;

use super::query_builder::{escape_ilike, WhereClauseBuilder};
use crate::{
    models::{User, UserId, UserListQuery, UserListSortBy},
    Error, Result,
};

/// User repository for database operations
#[derive(Clone)]
pub struct UserRepository {
    pool: PgPool,
}

impl UserRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Get the database pool
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Create a new user
    ///
    /// Relies on database UNIQUE constraints on `username` and `email` columns
    /// to prevent duplicates atomically (no TOCTOU race condition).
    pub async fn create(&self, user: &User) -> Result<User> {
        self.create_with_executor(user, &self.pool).await
    }

    /// Create a new user using a provided executor (pool or transaction)
    ///
    /// Relies on database UNIQUE constraints on `username` and `email` columns
    /// to prevent duplicates atomically (no TOCTOU race condition).
    pub async fn create_with_executor<'e, E>(&self, user: &User, executor: E) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let u = sqlx::query_as::<_, User>(
            r"
            INSERT INTO users (id, username, email, password_hash, signup_method, role, status, email_verified, created_at, updated_at, password_changed_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user.id.as_str())
        .bind(&user.username)
        .bind(user.email.as_ref())
        .bind(&user.password_hash)
        .bind(user.signup_method)
        .bind(user.role)
        .bind(user.status)
        .bind(user.email_verified)
        .bind(user.created_at)
        .bind(user.updated_at)
        .fetch_one(executor)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
                let constraint = db_err.constraint().unwrap_or("");
                if constraint.contains("username") {
                    Error::AlreadyExists("Username already taken".to_string())
                } else if constraint.contains("email") {
                    Error::AlreadyExists("Email already taken".to_string())
                } else {
                    Error::AlreadyExists("Username or email already taken".to_string())
                }
            }
            _ => Error::Database(e),
        })?;

        Ok(u)
    }

    /// Get user by ID
    pub async fn get_by_id(&self, user_id: &UserId) -> Result<Option<User>> {
        let u = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
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
        let u = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE id = $1 AND deleted_at IS NULL
            FOR UPDATE
            ",
        )
        .bind(user_id.as_str())
        .fetch_optional(executor)
        .await?;

        Ok(u)
    }

    /// Get multiple users by IDs in a single batch query
    pub async fn get_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<&str> = user_ids
            .iter()
            .map(super::super::models::id::UserId::as_str)
            .collect();
        let users = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE id = ANY($1) AND deleted_at IS NULL
            ",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await?;

        Ok(users)
    }

    /// Get user by username
    pub async fn get_by_username(&self, username: &str) -> Result<Option<User>> {
        let u = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE username = $1 AND deleted_at IS NULL
            ",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;

        Ok(u)
    }

    /// Get user by email
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        let u = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE LOWER(email) = LOWER($1) AND deleted_at IS NULL
            ",
        )
        .bind(email)
        .fetch_optional(&self.pool)
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
        self.update_with_executor(user, old_version, &self.pool)
            .await
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
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET username = $2, email = $3, password_hash = $4, role = $5, status = $6,
                email_verified = $7, updated_at = $8, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL AND version = $9
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user.id.as_str())
        .bind(&user.username)
        .bind(user.email.as_ref())
        .bind(&user.password_hash)
        .bind(user.role)
        .bind(user.status)
        .bind(user.email_verified)
        .bind(Utc::now())
        .bind(old_version)
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
                Err(Error::NotFound(format!(
                    "User {} not found",
                    user.id.as_str()
                )))
            }
        }
    }

    /// Update the user profile atomically with optimistic locking.
    ///
    /// Supports updating username alone, password alone, or both in one write.
    /// When `password_hash` is `Some`, the password metadata is updated and
    /// `password_version` is incremented exactly once in the same statement.
    pub async fn update_profile_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        username: &str,
        password_hash: Option<&str>,
        old_version: i32,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET username = $2,
                password_hash = COALESCE($3, password_hash),
                updated_at = $4,
                password_changed_at = CASE
                    WHEN $3 IS NULL THEN password_changed_at
                    ELSE $4
                END,
                password_version = CASE
                    WHEN $3 IS NULL THEN password_version
                    ELSE password_version + 1
                END,
                version = version + 1
            WHERE id = $1 AND deleted_at IS NULL AND version = $5
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(username)
        .bind(password_hash)
        .bind(now)
        .bind(old_version)
        .fetch_optional(executor)
        .await?;

        if let Some(updated) = u {
            Ok(updated)
        } else {
            let exists = self.get_by_id(user_id).await?.is_some();
            if exists {
                Err(Error::OptimisticLockConflict)
            } else {
                Err(Error::NotFound(format!(
                    "User {} not found",
                    user_id.as_str()
                )))
            }
        }
    }

    /// Soft delete user
    pub async fn delete(&self, user_id: &UserId) -> Result<bool> {
        self.delete_with_executor(user_id, &self.pool).await
    }

    /// Soft delete user using a provided executor (pool or transaction)
    pub async fn delete_with_executor<'e, E>(&self, user_id: &UserId, executor: E) -> Result<bool>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let result = sqlx::query(
            r"
            UPDATE users
            SET deleted_at = $2, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(user_id.as_str())
        .bind(Utc::now())
        .execute(executor)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Update user password
    pub async fn update_password(&self, user_id: &UserId, password_hash: &str) -> Result<User> {
        self.update_password_with_executor(user_id, password_hash, &self.pool)
            .await
    }

    /// Update user password using a provided executor (pool or transaction)
    pub async fn update_password_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        password_hash: &str,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let now = Utc::now();
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET password_hash = $2, updated_at = $3, password_changed_at = $3, password_version = password_version + 1, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(password_hash)
        .bind(now)
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {} not found", user_id.as_str())))?;

        Ok(u)
    }

    /// Update user email verification status
    pub async fn update_email_verified(
        &self,
        user_id: &UserId,
        email_verified: bool,
    ) -> Result<User> {
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET email_verified = $2, updated_at = $3, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(email_verified)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {} not found", user_id.as_str())))?;

        Ok(u)
    }

    /// Update user status (Active/Pending/Banned)
    pub async fn update_status(
        &self,
        user_id: &UserId,
        status: crate::models::UserStatus,
    ) -> Result<User> {
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET status = $2, updated_at = $3, version = version + 1
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(status)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::NotFound(format!("User {} not found", user_id.as_str())))?;

        Ok(u)
    }

    /// Build the shared WHERE clause conditions for user list queries.
    fn build_user_list_conditions(query: &UserListQuery) -> WhereClauseBuilder {
        let mut wb = WhereClauseBuilder::new();
        wb.push_literal("deleted_at IS NULL");

        if query.search.is_some() {
            wb.push_param("(username ILIKE ${idx} OR email ILIKE ${idx})");
        }
        if query.status.is_some() {
            wb.push_param("status = ${idx}");
        }
        if query.role.is_some() {
            wb.push_param("role = ${idx}");
        }

        wb
    }

    fn build_order_by(query: &UserListQuery) -> String {
        let direction = query.sort_direction.as_sql();
        match query.sort_by {
            UserListSortBy::Username => format!("username {direction}, id {direction}"),
            UserListSortBy::Email => format!("email {direction} NULLS LAST, id {direction}"),
            UserListSortBy::Status => {
                format!("status {direction}, created_at {direction}, id {direction}")
            }
            UserListSortBy::Role => {
                format!("role {direction}, created_at {direction}, id {direction}")
            }
            UserListSortBy::UpdatedAt => format!("updated_at {direction}, id {direction}"),
            UserListSortBy::CreatedAt => format!("created_at {direction}, id {direction}"),
        }
    }

    /// Bind the filter parameters (search, status, role) onto a sqlx query in order.
    /// This is used by both count and list queries to avoid duplicating bind logic.
    fn bind_user_filters<'q, O>(
        mut qb: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
        search_pattern: Option<&'q String>,
        status_enum: Option<&'q crate::models::UserStatus>,
        role_enum: Option<&'q crate::models::UserRole>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        if let Some(pattern) = search_pattern {
            qb = qb.bind(pattern);
        }
        if let Some(status) = status_enum {
            qb = qb.bind(status);
        }
        if let Some(role) = role_enum {
            qb = qb.bind(role);
        }
        qb
    }

    /// List users with pagination
    pub async fn list(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        let limit = i64::try_from(query.pagination.limit()).unwrap_or(i64::MAX);
        let offset = i64::try_from(query.pagination.offset()).unwrap_or(i64::MAX);

        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));

        let wb = Self::build_user_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!("SELECT COUNT(*) as count FROM users WHERE {count_where}");

        // We need to use query_scalar which returns a different type, so bind manually
        let mut count_qb = sqlx::query_scalar::<_, i64>(&count_sql);
        if let Some(ref pattern) = search_pattern {
            count_qb = count_qb.bind(pattern);
        }
        if let Some(ref status) = &query.status {
            count_qb = count_qb.bind(status);
        }
        if let Some(ref role) = &query.role {
            count_qb = count_qb.bind(role);
        }
        let count: i64 = count_qb.fetch_one(&self.pool).await?;

        // List query: $1=LIMIT, $2=OFFSET, then filters start at $3
        let (list_where, _) = wb.build(3);
        let order_by = Self::build_order_by(query);
        let list_sql = format!(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, password_changed_at, password_version, version, deleted_at, email_verified
            FROM users
            WHERE {list_where}
            ORDER BY {order_by}
            LIMIT $1 OFFSET $2
            "
        );

        // Use query_as with the shared bind helper
        let list_qb = sqlx::query_as::<_, User>(&list_sql)
            .bind(limit)
            .bind(offset);
        let list_qb = Self::bind_user_filters(
            list_qb,
            search_pattern.as_ref(),
            query.status.as_ref(),
            query.role.as_ref(),
        );
        let users: Vec<User> = list_qb.fetch_all(&self.pool).await?;

        Ok((users, count))
    }

    /// List admin-capable users (root + admin) with pagination.
    pub async fn list_admins(&self, query: &UserListQuery) -> Result<(Vec<User>, i64)> {
        let limit = i64::try_from(query.pagination.limit()).unwrap_or(i64::MAX);
        let offset = i64::try_from(query.pagination.offset()).unwrap_or(i64::MAX);
        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let order_by = Self::build_order_by(query);

        let mut count_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT COUNT(*) FROM users WHERE deleted_at IS NULL AND role IN (",
        );
        count_qb.push_bind(crate::models::UserRole::Root);
        count_qb.push(", ");
        count_qb.push_bind(crate::models::UserRole::Admin);
        count_qb.push(")");
        if let Some(pattern) = &search_pattern {
            count_qb.push(" AND (username ILIKE ");
            count_qb.push_bind(pattern.clone());
            count_qb.push(" OR email ILIKE ");
            count_qb.push_bind(pattern.clone());
            count_qb.push(")");
        }
        let count = count_qb
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await?;

        let mut list_qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, username, email, password_hash, signup_method, role, status, created_at, \
             updated_at, password_changed_at, password_version, version, deleted_at, \
             email_verified \
             FROM users WHERE deleted_at IS NULL AND role IN (",
        );
        list_qb.push_bind(crate::models::UserRole::Root);
        list_qb.push(", ");
        list_qb.push_bind(crate::models::UserRole::Admin);
        list_qb.push(")");
        if let Some(pattern) = &search_pattern {
            list_qb.push(" AND (username ILIKE ");
            list_qb.push_bind(pattern.clone());
            list_qb.push(" OR email ILIKE ");
            list_qb.push_bind(pattern.clone());
            list_qb.push(")");
        }
        list_qb.push(format!(" ORDER BY {order_by} LIMIT "));
        list_qb.push_bind(limit);
        list_qb.push(" OFFSET ");
        list_qb.push_bind(offset);

        let users = list_qb
            .build_query_as::<User>()
            .fetch_all(&self.pool)
            .await?;

        Ok((users, count))
    }

    /// Check if username exists
    pub async fn username_exists(&self, username: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) as count
            FROM users
            WHERE username = $1 AND deleted_at IS NULL
            ",
        )
        .bind(username)
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Check if email exists
    pub async fn email_exists(&self, email: &str) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            r"
            SELECT COUNT(*) as count
            FROM users
            WHERE email = $1 AND deleted_at IS NULL
            ",
        )
        .bind(email)
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SignupMethod;
    use synctv_core_testing::create_test_pool;

    #[test]
    fn test_build_user_list_conditions_no_filters() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        let (sql, next_idx) = wb.build(1);
        assert_eq!(sql, "deleted_at IS NULL");
        assert_eq!(next_idx, 1); // no params consumed
        assert_eq!(wb.param_count(), 0);
    }

    #[test]
    fn test_build_user_list_conditions_with_search() {
        let query = UserListQuery {
            search: Some("alice".to_string()),
            status: None,
            role: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, next_idx) = wb.build(1);
        assert!(sql.contains("username ILIKE"));
        assert!(sql.contains("email ILIKE"));
        assert_eq!(next_idx, 2);
    }

    #[test]
    fn test_build_user_list_conditions_with_status() {
        let query = UserListQuery {
            search: None,
            status: Some(crate::models::UserStatus::Active),
            role: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, _) = wb.build(1);
        assert!(sql.contains("status = $1"));
    }

    #[test]
    fn test_build_user_list_conditions_with_role() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: Some(crate::models::UserRole::Admin),
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 1);
        let (sql, _) = wb.build(1);
        assert!(sql.contains("role = $1"));
    }

    #[test]
    fn test_build_user_list_conditions_all_filters() {
        let query = UserListQuery {
            search: Some("bob".to_string()),
            status: Some(crate::models::UserStatus::Active),
            role: Some(crate::models::UserRole::User),
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::CreatedAt,
            sort_direction: crate::models::SortDirection::Desc,
        };
        let wb = UserRepository::build_user_list_conditions(&query);

        assert_eq!(wb.param_count(), 3);

        // Count query: params start at $1
        let (sql, next) = wb.build(1);
        assert!(sql.contains("deleted_at IS NULL"));
        assert!(sql.contains("username ILIKE $1"));
        assert!(sql.contains("status = $2"));
        assert!(sql.contains("role = $3"));
        assert_eq!(next, 4);

        // List query: params start at $3 (after LIMIT/OFFSET)
        let (sql, next) = wb.build(3);
        assert!(sql.contains("username ILIKE $3"));
        assert!(sql.contains("status = $4"));
        assert!(sql.contains("role = $5"));
        assert_eq!(next, 6);
    }

    #[test]
    fn test_build_order_by_uses_requested_sort() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            pagination: crate::models::PageParams::default(),
            sort_by: crate::models::UserListSortBy::Username,
            sort_direction: crate::models::SortDirection::Asc,
        };

        assert_eq!(
            UserRepository::build_order_by(&query),
            "username ASC, id ASC"
        );
    }

    #[test]
    fn test_user_list_order_clause_supports_username_ascending() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            sort_by: crate::models::UserListSortBy::Username,
            sort_direction: crate::models::SortDirection::Asc,
            pagination: crate::models::PageParams::default(),
        };

        assert_eq!(
            UserRepository::build_order_by(&query),
            "username ASC, id ASC"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new(
            "testuser".into(),
            Some("test@example.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        let created = repo.create(&user).await.unwrap();
        assert_eq!(created.username, "testuser");
        assert_eq!(created.email, Some("test@example.com".into()));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_user_duplicate_username_returns_already_exists() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user1 = User::new(
            "same_name".into(),
            Some("a@b.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        repo.create(&user1).await.unwrap();
        let user2 = User::new(
            "same_name".into(),
            Some("c@d.com".into()),
            "hash".into(),
            SignupMethod::Email,
        );
        let err = repo.create(&user2).await.unwrap_err();
        assert!(matches!(err, Error::AlreadyExists(_)));
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_soft_delete_user() {
        let (_postgres, pool) = create_test_pool().await;
        let repo = UserRepository::new(pool.clone());
        let user = User::new("deleteme".into(), None, "hash".into(), SignupMethod::Email);
        let created = repo.create(&user).await.unwrap();
        assert!(repo.delete(&created.id).await.unwrap());
        // Soft-deleted users should not be returned by get_by_id
        assert!(repo.get_by_id(&created.id).await.unwrap().is_none());
    }
}
