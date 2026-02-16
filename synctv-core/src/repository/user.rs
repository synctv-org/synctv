use std::str::FromStr;
use chrono::Utc;
use sqlx::PgPool;

use crate::{
    models::{User, UserId, UserListQuery},
    Error, Result,
};
use super::query_builder::{WhereClauseBuilder, escape_ilike};

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
        let u = sqlx::query_as::<_, User>(
            r"
            INSERT INTO users (id, username, email, password_hash, signup_method, role, status, email_verified, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            ",
        )
        .bind(user.id.as_str())
        .bind(&user.username)
        .bind(user.email.as_ref())
        .bind(&user.password_hash)
        .bind(user.signup_method.map(|m| m.as_str()))
        .bind(user.role)
        .bind(user.status)
        .bind(user.email_verified)
        .bind(user.created_at)
        .bind(user.updated_at)
        .fetch_one(&self.pool)
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
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            FROM users
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(user_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(u)
    }

    /// Get multiple users by IDs in a single batch query
    pub async fn get_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<&str> = user_ids.iter().map(super::super::models::id::UserId::as_str).collect();
        let users = sqlx::query_as::<_, User>(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
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
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
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
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            FROM users
            WHERE email = $1 AND deleted_at IS NULL
            ",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;

        Ok(u)
    }

    /// Update user
    pub async fn update(&self, user: &User) -> Result<User> {
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET username = $2, email = $3, password_hash = $4, role = $5, status = $6, updated_at = $7
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            ",
        )
        .bind(user.id.as_str())
        .bind(&user.username)
        .bind(user.email.as_ref())
        .bind(&user.password_hash)
        .bind(user.role)
        .bind(user.status)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await?;

        Ok(u)
    }

    /// Soft delete user
    pub async fn delete(&self, user_id: &UserId) -> Result<bool> {
        let result = sqlx::query(
            r"
            UPDATE users
            SET deleted_at = $2
            WHERE id = $1 AND deleted_at IS NULL
            ",
        )
        .bind(user_id.as_str())
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Update user password
    pub async fn update_password(&self, user_id: &UserId, password_hash: &str) -> Result<User> {
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET password_hash = $2, updated_at = $3
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(password_hash)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await?;

        Ok(u)
    }

    /// Update user email verification status
    pub async fn update_email_verified(&self, user_id: &UserId, email_verified: bool) -> Result<User> {
        let u = sqlx::query_as::<_, User>(
            r"
            UPDATE users
            SET email_verified = $2, updated_at = $3
            WHERE id = $1 AND deleted_at IS NULL
            RETURNING id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            ",
        )
        .bind(user_id.as_str())
        .bind(email_verified)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await?;

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

    /// Bind the filter parameters (search, status, role) onto a sqlx query in order.
    /// This is used by both count and list queries to avoid duplicating bind logic.
    fn bind_user_filters<'q, O>(
        mut qb: sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
        search_pattern: &'q Option<String>,
        status_enum: &'q Option<crate::models::UserStatus>,
        role_enum: &'q Option<crate::models::UserRole>,
    ) -> sqlx::query::QueryAs<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>,
    {
        if let Some(ref pattern) = search_pattern {
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
        let limit = query.pagination.limit() as i64;
        let offset = query.pagination.offset() as i64;

        let search_pattern = query.search.as_ref().map(|s| escape_ilike(s));
        let status_enum = query.status.as_ref()
            .map(|s| crate::models::UserStatus::from_str(s).map_err(crate::Error::InvalidInput))
            .transpose()?;
        let role_enum = query.role.as_ref()
            .map(|s| crate::models::UserRole::from_str(s).map_err(crate::Error::InvalidInput))
            .transpose()?;

        let wb = Self::build_user_list_conditions(query);

        // Count query: params start at $1
        let (count_where, _) = wb.build(1);
        let count_sql = format!(
            "SELECT COUNT(*) as count FROM users WHERE {count_where}"
        );

        // We need to use query_scalar which returns a different type, so bind manually
        let mut count_qb = sqlx::query_scalar::<_, i64>(&count_sql);
        if let Some(ref pattern) = search_pattern {
            count_qb = count_qb.bind(pattern);
        }
        if let Some(ref status) = status_enum {
            count_qb = count_qb.bind(status);
        }
        if let Some(ref role) = role_enum {
            count_qb = count_qb.bind(role);
        }
        let count: i64 = count_qb.fetch_one(&self.pool).await?;

        // List query: $1=LIMIT, $2=OFFSET, then filters start at $3
        let (list_where, _) = wb.build(3);
        let list_sql = format!(
            r"
            SELECT id, username, email, password_hash, signup_method, role, status, created_at, updated_at, deleted_at, email_verified
            FROM users
            WHERE {list_where}
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "
        );

        // Use query_as with the shared bind helper
        let list_qb = sqlx::query_as::<_, User>(&list_sql)
            .bind(limit)
            .bind(offset);
        let list_qb = Self::bind_user_filters(list_qb, &search_pattern, &status_enum, &role_enum);
        let users: Vec<User> = list_qb.fetch_all(&self.pool).await?;

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

    #[test]
    fn test_build_user_list_conditions_no_filters() {
        let query = UserListQuery {
            search: None,
            status: None,
            role: None,
            pagination: crate::models::PageParams::default(),
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
            status: Some("active".to_string()),
            role: None,
            pagination: crate::models::PageParams::default(),
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
            role: Some("admin".to_string()),
            pagination: crate::models::PageParams::default(),
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
            status: Some("active".to_string()),
            role: Some("user".to_string()),
            pagination: crate::models::PageParams::default(),
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

    // ========== Integration Tests (Require DB) ==========

    #[tokio::test]
    #[ignore = "Requires database"]
    async fn test_create_user() {
        // Would connect to test DB and verify:
        // let pool = PgPool::connect("postgresql://...").await.unwrap();
        // let repo = UserRepository::new(pool);
        // let user = User::new("testuser".into(), Some("test@example.com".into()), "hash".into(), None);
        // let created = repo.create(&user).await.unwrap();
        // assert_eq!(created.username, "testuser");
        // assert_eq!(created.email, Some("test@example.com".into()));
    }

    #[tokio::test]
    #[ignore = "Requires database"]
    async fn test_create_user_duplicate_username_returns_already_exists() {
        // Would connect to test DB and verify:
        // let pool = PgPool::connect("postgresql://...").await.unwrap();
        // let repo = UserRepository::new(pool);
        // let user1 = User::new("same_name".into(), Some("a@b.com".into()), "hash".into(), None);
        // repo.create(&user1).await.unwrap();
        // let user2 = User::new("same_name".into(), Some("c@d.com".into()), "hash".into(), None);
        // let err = repo.create(&user2).await.unwrap_err();
        // assert!(matches!(err, Error::AlreadyExists(_)));
    }

    #[tokio::test]
    #[ignore = "Requires database"]
    async fn test_soft_delete_user() {
        // Would connect to test DB and verify soft delete:
        // let pool = PgPool::connect("postgresql://...").await.unwrap();
        // let repo = UserRepository::new(pool);
        // let user = User::new("deleteme".into(), None, "hash".into(), None);
        // let created = repo.create(&user).await.unwrap();
        // assert!(repo.delete(&created.id).await.unwrap());
        // assert!(repo.get_by_id(&created.id).await.unwrap().is_none()); // soft-deleted users not returned
    }
}
