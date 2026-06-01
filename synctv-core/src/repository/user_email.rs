use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::user::USER_SELECT_COLUMNS;
use crate::{
    models::{User, UserId},
    Error, Result,
};

#[derive(Debug, Clone)]
pub struct UserWithEmail {
    pub user: User,
    pub email: Option<String>,
}

struct UserWithEmailRow {
    user: User,
    email: Option<String>,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for UserWithEmailRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;

        Ok(Self {
            user: User::from_row(row)?,
            email: row.try_get("email")?,
        })
    }
}

impl From<UserWithEmailRow> for UserWithEmail {
    fn from(row: UserWithEmailRow) -> Self {
        Self {
            user: row.user,
            email: row.email,
        }
    }
}

#[derive(Clone)]
pub struct UserEmailRepository {
    pool: PgPool,
}

impl UserEmailRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_for_user_with_executor<'e, E>(
        &self,
        user: &User,
        email: Option<&str>,
        executor: E,
    ) -> Result<Option<String>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let Some(email) = email.filter(|email| !email.trim().is_empty()) else {
            return Ok(None);
        };

        let inserted = sqlx::query_scalar::<_, String>(
            r"
            INSERT INTO auth_email_identities (
                user_id, email, created_at, updated_at
            )
            VALUES ($1, $2, $3, $4)
            RETURNING email
            ",
        )
        .bind(user.id)
        .bind(email)
        .bind(user.created_at)
        .bind(user.updated_at)
        .fetch_one(executor)
        .await
        .map_err(map_email_identity_error)?;

        Ok(Some(inserted))
    }

    pub async fn upsert_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        email: &str,
        now: DateTime<Utc>,
        executor: E,
    ) -> Result<UserWithEmail>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET updated_at = $3, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING *
            ),
            updated_email AS (
                INSERT INTO auth_email_identities (
                    user_id, email, created_at, updated_at
                )
                SELECT id, $2, $3, $3
                FROM updated_user
                ON CONFLICT (user_id)
                DO UPDATE SET
                    email = EXCLUDED.email,
                    updated_at = EXCLUDED.updated_at
                RETURNING user_id, email
            )
            SELECT {USER_SELECT_COLUMNS}, updated_email.email
            FROM updated_user u
            LEFT JOIN updated_email ON updated_email.user_id = u.id
            "
        );

        sqlx::query_as::<_, UserWithEmailRow>(&sql)
            .bind(user_id)
            .bind(email)
            .bind(now)
            .fetch_optional(executor)
            .await
            .map_err(map_email_identity_error)?
            .map(Into::into)
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    pub async fn delete_with_executor<'e, E>(
        &self,
        user_id: &UserId,
        now: DateTime<Utc>,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            WITH updated_user AS (
                UPDATE users
                SET updated_at = $2, version = version + 1
                WHERE id = $1 AND deleted_at IS NULL
                RETURNING *
            ),
            deleted_email AS (
                DELETE FROM auth_email_identities
                USING updated_user
                WHERE auth_email_identities.user_id = updated_user.id
            )
            SELECT {USER_SELECT_COLUMNS}
            FROM updated_user u
            "
        );

        sqlx::query_as::<_, User>(&sql)
            .bind(user_id)
            .bind(now)
            .fetch_optional(executor)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {user_id} not found")))
    }

    pub async fn get_email(&self, user_id: &UserId) -> Result<Option<String>> {
        sqlx::query_scalar::<_, String>(
            r"
            SELECT aei.email
            FROM auth_email_identities aei
            JOIN users u ON u.id = aei.user_id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            ",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Error::Database)
    }

    pub async fn get_by_user_id(&self, user_id: &UserId) -> Result<Option<UserWithEmail>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}, aei.email
            FROM users u
            LEFT JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE u.id = $1 AND u.deleted_at IS NULL
            "
        );

        let row = sqlx::query_as::<_, UserWithEmailRow>(&sql)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn get_by_email(&self, email: &str) -> Result<Option<UserWithEmail>> {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}, aei.email
            FROM users u
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "
        );

        let row = sqlx::query_as::<_, UserWithEmailRow>(&sql)
            .bind(email)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn get_by_email_with_executor<'e, E>(
        &self,
        email: &str,
        executor: E,
    ) -> Result<Option<UserWithEmail>>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let sql = format!(
            r"
            SELECT {USER_SELECT_COLUMNS}, aei.email
            FROM users u
            JOIN auth_email_identities aei ON aei.user_id = u.id
            WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            "
        );

        let row = sqlx::query_as::<_, UserWithEmailRow>(&sql)
            .bind(email)
            .fetch_optional(executor)
            .await?;
        Ok(row.map(Into::into))
    }

    pub async fn email_exists(&self, email: &str) -> Result<bool> {
        sqlx::query_scalar::<_, bool>(
            r"
            SELECT EXISTS(
                SELECT 1
                FROM auth_email_identities aei
                JOIN users u ON u.id = aei.user_id
                WHERE LOWER(aei.email) = LOWER($1) AND u.deleted_at IS NULL
            )
            ",
        )
        .bind(email)
        .fetch_one(&self.pool)
        .await
        .map_err(Error::Database)
    }
}

fn map_email_identity_error(error: sqlx::Error) -> Error {
    match error {
        sqlx::Error::Database(ref db_err) if db_err.constraint().is_some() => {
            let constraint = db_err.constraint().unwrap_or("");
            if constraint.contains("email") {
                Error::AlreadyExists("Email already taken".to_string())
            } else {
                Error::AlreadyExists(
                    synctv_common::messages::USERNAME_OR_EMAIL_ALREADY_TAKEN.to_string(),
                )
            }
        }
        other => Error::Database(other),
    }
}
